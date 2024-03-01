mod model;

use std::{
    cmp::Ordering,
    collections::{
        btree_map,
        BTreeMap,
    },
    path::{
        Path,
        PathBuf,
    },
};

use async_compression::tokio::bufread::GzipDecoder;
use csv_async::{
    AsyncReaderBuilder,
    DeserializeRecordsIntoStream,
};
use futures::TryStreamExt;
use lazy_static::lazy_static;
use regex::Regex;
use serde::Deserialize;
use tokio::{
    fs::File,
    io::BufReader,
};

pub use self::model::{
    astro::AstrophysicalParameters,
    source::GaiaSource,
};
use crate::Error;

lazy_static! {
    static ref FILE_NAME_REGEX: Regex = r"^(\w+)_(\d+)-(\d+).csv.gz$".parse().unwrap();
}

pub struct Data {
    partitions: BTreeMap<u32, Partition>,
}

impl Data {
    pub async fn open(path: impl AsRef<Path>) -> Result<Self, Error> {
        fn parse_file_name(file_name: &str) -> Option<(&str, HealPixRange)> {
            let captures = FILE_NAME_REGEX.captures(file_name)?;
            let prefix = captures.get(1)?.as_str();
            let start = captures.get(2)?.as_str().parse().ok()?;
            let end = captures.get(2)?.as_str().parse().ok()?;
            Some((prefix, HealPixRange { start, end }))
        }

        let mut read_dir = tokio::fs::read_dir(path).await?;

        let mut partitions = BTreeMap::new();

        while let Some(entry) = read_dir.next_entry().await? {
            let Ok(file_name) = entry.file_name().into_string()
            else {
                continue;
            };
            let Some((prefix, healpix_range)) = parse_file_name(&file_name)
            else {
                continue;
            };

            let partition = partitions.entry(healpix_range.start).or_insert_with(|| {
                Partition {
                    healpix_range,
                    gaia_source: None,
                    astrophysical_parameters: None,
                }
            });

            let path = entry.path();

            match prefix {
                "GaiaSource" => partition.gaia_source = Some(path),
                "AstrophysicalParameters" => partition.astrophysical_parameters = Some(path),
                _ => continue,
            }
        }

        Ok(Self { partitions })
    }

    pub fn records<'a>(&'a self) -> Records<'a> {
        Records::new(self)
    }
}

#[derive(Clone, Copy, Debug)]
pub struct HealPixRange {
    pub start: u32,
    pub end: u32,
}

#[derive(Debug)]
struct Partition {
    healpix_range: HealPixRange,
    gaia_source: Option<PathBuf>,
    astrophysical_parameters: Option<PathBuf>,
}

type Csv<T> = DeserializeRecordsIntoStream<'static, GzipDecoder<BufReader<File>>, T>;

#[derive(Default)]
struct Readers {
    gaia_source: Option<Csv<GaiaSource>>,
    astrophysical_parameters: Option<Csv<AstrophysicalParameters>>,
}

#[derive(Clone, Debug, Default)]
struct Buffers {
    gaia_source: Option<GaiaSource>,
    astrophysical_parameters: Option<AstrophysicalParameters>,
}

#[derive(Clone, Debug)]
pub struct Record {
    pub healpix_range: HealPixRange,
    pub gaia_source: GaiaSource,
    pub astrophysical_parameters: Option<AstrophysicalParameters>,
}

pub struct Records<'a> {
    partitions_iter: btree_map::Iter<'a, u32, Partition>,
    num_partitions: usize,
    current_healpix_range: Option<HealPixRange>,
    readers: Readers,
    buffers: Buffers,
}

impl<'a> Records<'a> {
    fn new(data: &'a Data) -> Self {
        Self {
            partitions_iter: data.partitions.iter(),
            num_partitions: data.partitions.len(),
            current_healpix_range: None,
            readers: Default::default(),
            buffers: Default::default(),
        }
    }

    pub async fn read_record(&mut self) -> Result<Option<Record>, Error> {
        async fn open_reader<T: for<'de> Deserialize<'de> + 'static>(
            path: &Path,
        ) -> Result<Csv<T>, Error> {
            let file = File::open(path).await?;
            let reader = BufReader::new(file);
            let gzip_reader = GzipDecoder::new(reader);
            let stream = AsyncReaderBuilder::new()
                .comment(Some(b'#'))
                .delimiter(b',')
                .create_deserializer(gzip_reader)
                .into_deserialize();
            Ok(stream)
        }

        async fn read_record<T: for<'de> Deserialize<'de> + 'static>(
            reader_opt: &mut Option<Csv<T>>,
        ) -> Result<Option<T>, Error> {
            if let Some(reader) = reader_opt {
                if let Some(record) = reader.try_next().await? {
                    Ok(Some(record))
                }
                else {
                    *reader_opt = None;
                    Ok(None)
                }
            }
            else {
                Ok(None)
            }
        }

        loop {
            // open next partition, if there are no readers
            if self.readers.gaia_source.is_none() && self.readers.astrophysical_parameters.is_none()
            {
                if let Some((_, partition)) = self.partitions_iter.next() {
                    self.current_healpix_range = Some(partition.healpix_range);

                    if let Some(gaia_source) = &partition.gaia_source {
                        self.readers.gaia_source = Some(open_reader(gaia_source).await?);
                    }

                    if let Some(astrophysical_parameters) = &partition.astrophysical_parameters {
                        self.readers.astrophysical_parameters =
                            Some(open_reader(astrophysical_parameters).await?);
                    }
                }
                else {
                    return Ok(None);
                }
            }

            match &mut self.buffers {
                Buffers {
                    gaia_source: None, ..
                } if self.readers.gaia_source.is_some() => {
                    // read GaiaSource
                    self.buffers.gaia_source = read_record(&mut self.readers.gaia_source).await?;
                }
                Buffers {
                    astrophysical_parameters: None,
                    ..
                } if self.readers.astrophysical_parameters.is_some() => {
                    // read astrophysical_parameters
                    self.buffers.astrophysical_parameters =
                        read_record(&mut self.readers.astrophysical_parameters).await?;
                }
                Buffers {
                    gaia_source: Some(gaia_source),
                    astrophysical_parameters: Some(astrophysical_parameters),
                } => {
                    match gaia_source
                        .source_id
                        .cmp(&astrophysical_parameters.source_id)
                    {
                        Ordering::Equal => {
                            return Ok(Some(Record {
                                healpix_range: self.current_healpix_range.unwrap(),
                                gaia_source: self.buffers.gaia_source.take().unwrap(),
                                astrophysical_parameters: self
                                    .buffers
                                    .astrophysical_parameters
                                    .take(),
                            }));
                        }
                        Ordering::Less => {
                            return Ok(Some(Record {
                                healpix_range: self.current_healpix_range.unwrap(),
                                gaia_source: self.buffers.gaia_source.take().unwrap(),
                                astrophysical_parameters: None,
                            }));
                        }
                        Ordering::Greater => {
                            // there should be an entry in GaiaSource for every record we find.
                            tracing::warn!(
                                source_id = astrophysical_parameters.source_id,
                                "missing GaiaSource"
                            );
                        }
                    }
                }
                Buffers {
                    gaia_source: None,
                    astrophysical_parameters: Some(astrophysical_parameters),
                } => {
                    // there should be an entry in GaiaSource for every record we find.
                    tracing::warn!(
                        source_id = astrophysical_parameters.source_id,
                        "missing GaiaSource"
                    );
                }
                Buffers {
                    gaia_source: Some(_),
                    astrophysical_parameters: None,
                } => {
                    return Ok(Some(Record {
                        healpix_range: self.current_healpix_range.unwrap(),
                        gaia_source: self.buffers.gaia_source.take().unwrap(),
                        astrophysical_parameters: self.buffers.astrophysical_parameters.take(),
                    }));
                }
                Buffers {
                    gaia_source: None,
                    astrophysical_parameters: None,
                } => {
                    // fetch next records
                }
            }
        }
    }

    pub fn skip_file(&mut self) {
        self.readers = Default::default();
    }

    pub fn progress(&self) -> (usize, usize) {
        let pos = self.num_partitions - self.partitions_iter.len();
        (pos, self.num_partitions)
    }
}
