use std::io::prelude::*;
use std::path::Path;
use std::{env, io};

use flowly_mp4::Mp4File;
use flowly_mp4::{error::MemoryStorageError, TrackType};
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

async fn samples<P: AsRef<Path>>(
    filename: &P,
) -> Result<(), flowly_mp4::Error<MemoryStorageError>> {
    let f = File::open(filename).await?;
    let mut reader = BufReader::new(f);

    let mut mp4_file = Mp4File::new(&mut reader);
    println!("streaming possible: {}", mp4_file.read_header().await?);

    let mut keys = mp4_file
        .tracks
        .iter()
        .filter(|&(_, v)| v.track_type() == TrackType::Video)
        .map(|(k, _)| *k);

    let track_id = keys.next().unwrap();
    let samples_len = mp4_file.tracks.get(&track_id).unwrap().samples.len();

    for idx in 0..samples_len {
        let samp = mp4_file.tracks.get(&track_id).unwrap().samples[idx].clone();

        let data = mp4_file
            .read_sample_data(track_id, idx)
            .await?
            .map(|x| x.slice(0..16));

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
