use std::io::prelude::*;
use std::path::Path;
use std::{env, io};

use mp4::{MemoryStorageError, TrackType};
use tokio::fs::File;
use tokio::io::BufReader;

#[tokio::main]
async fn main() {
    let args: Vec<String> = env::args().collect();

    if args.len() < 2 {
        println!("Usage: mp4sample <filename>");
        std::process::exit(1);
    }

    if let Err(err) = samples(&args[1]).await {
        let _ = writeln!(io::stderr(), "{}", err);
    }
}

async fn samples<P: AsRef<Path>>(filename: &P) -> Result<(), mp4::Error<MemoryStorageError>> {
    let f = File::open(filename).await?;
    let mut reader = BufReader::new(f);

    let mut mp4_file = mp4::Mp4File::new(&mut reader);
    println!("streaming possible: {}", mp4_file.read_header().await?);

    let mut keys = mp4_file
        .tracks
        .iter()
        .filter(|&(_, v)| v.track_type() == TrackType::Video)
        .map(|(k, _)| *k);

    let track_id = keys.next().unwrap();
    let track = mp4_file.tracks.get(&track_id).unwrap();

    for (idx, samp) in track.samples.iter().enumerate() {
        let data = mp4_file
            .read_sample_data(track_id, idx)
            .await?
            .map(|x| x.slice(0..32));

        println!(
            "[{} {} {}] {} - <{}> {} +{} {:?}",
            idx + 1,
            samp.chunk_id,
            samp.offset,
            samp.is_sync,
            samp.size,
            samp.start_time,
            samp.rendering_offset,
            data.as_deref()
        );
    }

    Ok(())
}
