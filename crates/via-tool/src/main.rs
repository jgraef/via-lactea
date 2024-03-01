#![feature(btree_cursors)]

mod gaia;
mod gaiasky;
mod render;
mod utils;

use std::path::PathBuf;

use color_eyre::eyre::Error;
use gaia::Data;
use structopt::StructOpt;

use crate::gaiasky::load_gaia_sky;

#[derive(Debug, StructOpt)]
pub struct Args {
    #[structopt(long, env = "DATABASE_URL")]
    database_url: String,

    #[structopt(subcommand)]
    command: Command,
}

#[derive(Debug, StructOpt)]
enum Command {
    LoadGaiaSky {
        path: PathBuf,
    },
    Render {
        #[structopt(short, long)]
        output: PathBuf,
        path: PathBuf,
        #[structopt(short, long, default_value = "top-down")]
        view: render::View,
        #[structopt(short, long, default_value = "1024")]
        width: u32,
    },
    Export {
        #[structopt(short, long)]
        output: PathBuf,
        path: PathBuf,
        #[structopt(short, long, default_value = "1024")]
        limit_per_file: u64,
    },
    Test {
        path: PathBuf,
    },
}

impl Args {
    async fn run(self) -> Result<(), Error> {
        //let mut db = PgPool::connect(&self.database_url).await?;

        match self.command {
            Command::LoadGaiaSky { path } => {
                load_gaia_sky(path).await?;
            }
            Command::Render {
                output,
                path,
                view,
                width,
            } => {
                render::render(output, path, view, width).await?;
            }
            Command::Export {
                output,
                path,
                limit_per_file,
            } => {
                render::export(output, path, limit_per_file).await?;
            }
            Command::Test { path } => {
                let data = Data::open(path).await?;
                let mut records = data.records();

                while let Some(record) = records.read_record().await? {
                    println!(
                        "{} - {}: {}, {:?}",
                        record.healpix_range.start,
                        record.healpix_range.end,
                        record.gaia_source.source_id,
                        record.astrophysical_parameters.map(|x| x.source_id),
                    )
                }
            }
        }

        Ok(())
    }
}

#[tokio::main]
async fn main() -> Result<(), Error> {
    dotenvy::dotenv().ok();
    color_eyre::install()?;
    tracing_subscriber::fmt::init();

    let args = Args::from_args();
    args.run().await?;

    Ok(())
}
