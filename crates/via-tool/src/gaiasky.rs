//! Catalogs can be found [here][1].
//!
//! [1]: https://gaia.ari.uni-heidelberg.de/gaiasky/repository/catalog/dr3/

use std::path::{
    Path,
    PathBuf,
};

use color_eyre::eyre::ensure;
use serde::{
    Deserialize,
    Serialize,
};
use tokio::{
    fs::{
        File,
        ReadDir,
    },
    io::{
        AsyncRead,
        AsyncReadExt,
        BufReader,
    },
};

use crate::Error;

#[derive(Debug, Deserialize)]
pub struct Data {
    pub loader: String,
    pub files: Vec<String>,
    pub epoch: u64,
}

#[derive(Debug, Deserialize)]
pub struct Manifest {
    pub key: String,
    pub name: String,
    pub version: u64,
    #[serde(rename = "type")]
    pub ty: String,
    #[serde(rename = "mingsversion")]
    pub min_gs_version: u64,
    pub description: String,
    #[serde(rename = "releasenotes")]
    pub release_notes: String,
    pub link: String,
    pub size: u64,
    #[serde(rename = "nobjects")]
    pub num_objects: u64,
    pub check: String,
    pub files: Vec<String>,
    pub data: Vec<Data>,
}

pub struct DataSet {
    path: PathBuf,
    manifest: Manifest,
}

impl DataSet {
    pub async fn open(path: impl AsRef<Path>) -> Result<Self, Error> {
        let path = path.as_ref();

        let manifest_json = tokio::fs::read(path.join("dataset.json")).await?;
        let manifest = serde_json::from_slice(&manifest_json)?;

        Ok(Self {
            path: path.to_owned(),
            manifest,
        })
    }

    pub fn manifest(&self) -> &Manifest {
        &self.manifest
    }

    pub async fn particles(&self) -> Result<ParticleDirReader, Error> {
        let file = self
            .manifest
            .data
            .iter()
            .flat_map(|data| &data.files)
            .find(|file| file.ends_with("/particles/"))
            .unwrap();
        let file = file.strip_prefix("$data/").unwrap();
        let file = &file[file.find('/').unwrap() + 1..];
        let path = self.path.join(file);

        tracing::debug!(path = %path.display(), "opening particle directory");

        Ok(ParticleDirReader::new(path).await?)
    }
}

pub struct ParticleDirReader {
    read_dir: ReadDir,
    file_reader: Option<ParticleFileReader<BufReader<File>>>,
}

impl ParticleDirReader {
    pub async fn new(path: impl AsRef<Path>) -> Result<Self, Error> {
        let read_dir = tokio::fs::read_dir(path).await?;

        Ok(Self {
            read_dir,
            file_reader: None,
        })
    }

    pub async fn read_particle(&mut self) -> Result<Option<Particle>, Error> {
        loop {
            if self.file_reader.is_none() {
                let Some(dir_entry) = self.read_dir.next_entry().await?
                else {
                    return Ok(None);
                };

                let path = dir_entry.path();
                tracing::debug!(path = %path.display(), "opening particle file");
                self.file_reader =
                    Some(ParticleFileReader::new(BufReader::new(File::open(path).await?)).await?);
            }

            let file_reader = self.file_reader.as_mut().unwrap();

            if let Some(particle) = file_reader.read_particle().await? {
                return Ok(Some(particle));
            }
            else {
                self.file_reader = None;
                //return Ok(None); // debug
            }
        }
    }
}

pub struct ParticleFileReader<R> {
    reader: R,
    num_read: u32,
    num_particles: u32,
}

impl<R: AsyncRead + Unpin> ParticleFileReader<R> {
    pub async fn new(mut reader: R) -> Result<Self, Error> {
        let tag = reader.read_i32().await?;
        ensure!(tag == -1, "invalid file tag");

        let version = reader.read_i32().await?;
        ensure!(version == 2, "unsupported version");

        let num_particles = reader.read_u32().await?;

        Ok(Self {
            reader,
            num_read: 0,
            num_particles,
        })
    }

    pub async fn read_particle(&mut self) -> Result<Option<Particle>, Error> {
        if self.num_read >= self.num_particles {
            return Ok(None);
        }

        let position = Vector3 {
            x: self.reader.read_f64().await?,
            y: self.reader.read_f64().await?,
            z: self.reader.read_f64().await?,
        };

        let proper_motion = Vector3 {
            x: self.reader.read_f32().await?,
            y: self.reader.read_f32().await?,
            z: self.reader.read_f32().await?,
        };

        let mu_alpha = self.reader.read_f32().await?;
        let mu_delta = self.reader.read_f32().await?;
        let radial_velocity = self.reader.read_f32().await?;
        let apparent_magnitude = self.reader.read_f32().await?;
        let absolute_magnitude = self.reader.read_f32().await?;
        let color = self.reader.read_f32().await?;
        let size = self.reader.read_f32().await?;
        let hip = self.reader.read_u32().await?;
        let source_id = self.reader.read_u64().await?;

        let color = {
            // public static int floatToIntColor (float value) {
            //     int intBits = Float.floatToRawIntBits(value);
            //     intBits |= (int)((intBits >>> 24) * (255f / 254f)) << 24;
            //     return intBits;
            // }

            fn u32_to_f32(x: u32) -> f32 {
                x as f32
            }
            fn f32_to_u32(x: f32) -> u32 {
                x as u32
            }

            let mut int_bits = color.to_bits();
            int_bits |= f32_to_u32(u32_to_f32(int_bits >> 24) * (255.0 / 254.0)) << 24;

            let r = (int_bits & 0xff) as u8;
            let g = ((int_bits >> 8) & 0xff) as u8;
            let b = ((int_bits >> 16) & 0xff) as u8;
            Color { r, g, b }
        };

        const SCALE: f64 = 1e9; // m per gaia-sky unit
        const STAR_SCALE: f32 = 1.31526e-6;

        let names_length = self.reader.read_u32().await?;
        let mut names = vec![];
        if names_length > 0 {
            let n = names_length as usize;
            let mut buf = String::new();

            for _ in 0..n {
                let c = self.reader.read_u16().await?;
                if c == b'|' as u16 {
                    if !buf.is_empty() {
                        names.push(std::mem::replace(&mut buf, String::new()));
                    }
                }
                else {
                    let c = char::from_u32(c as _).unwrap();
                    buf.push(c);
                }
            }
            if !buf.is_empty() {
                names.push(buf);
            }
        }

        self.num_read += 1;

        Ok(Some(Particle {
            position,
            proper_motion,
            mu_alpha,
            mu_delta,
            radial_velocity,
            apparent_magnitude,
            absolute_magnitude,
            color,
            size,
            hip,
            source_id,
            names,
        }))
    }
}

#[derive(Copy, Clone, Debug, Serialize, Deserialize)]
pub struct Color {
    pub r: u8,
    pub g: u8,
    pub b: u8,
}

#[derive(Copy, Clone, Debug, Serialize, Deserialize)]
pub struct Vector3<T> {
    pub x: T,
    pub y: T,
    pub z: T,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Particle {
    pub position: Vector3<f64>,
    pub proper_motion: Vector3<f32>,
    pub mu_alpha: f32,
    pub mu_delta: f32,
    pub radial_velocity: f32,
    pub apparent_magnitude: f32,
    pub absolute_magnitude: f32,
    pub color: Color,
    pub size: f32,
    pub hip: u32,
    pub source_id: u64,
    pub names: Vec<String>,
}

pub async fn load_gaia_sky(path: impl AsRef<Path>) -> Result<(), Error> {
    let dataset = DataSet::open(path).await?;
    let mut particles = dataset.particles().await?;

    while let Some(particle) = particles.read_particle().await? {
        //if particle.names.len() >= 2 {
        //    println!("{}: {:?}: {}, {}, {}", particle.source_id, particle.names,
        // particle.size, particle.size * 1.31526e-6, particle.size * 1.31526e-6 * 1e9);
        //}

        if particle.source_id == 147182172683187712 {
            println!("{particle:#?}");
            println!("size_scaled = {}", particle.size * 1.31526e-6);
            println!("size_in_m = {}", particle.size * 1.31526e-6 * 1e9);
            break;
        }

        //if let Some(name) = particle.names.first() {
        //    println!("{name}");
        //}
    }

    Ok(())
}
