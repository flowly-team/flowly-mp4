use std::io::prelude::*;
use std::path::Path;
use std::sync::Arc;
use std::{env, io};

use flowly::FrameFlags;
use flowly_mp4::{Error, Mp4File, Mp4FrameSource, TrackType};
use tokio::fs::File;
use tokio::io::BufReader;

use flowly_codec_openh264::Openh264Decoder;

#[tokio::main]
async fn main() {
    let args: Vec<String> = env::args().collect();

    if args.len() < 2 {
        println!("Usage: mp4sample <filename>");
        std::process::exit(1);
    }

    if let Err(err) = samples(&args[1]).await {
        let _ = writeln!(io::stderr(), "{err}");
    }
}

async fn samples<P: AsRef<Path>>(filename: &P) -> Result<(), Error> {
    let mut decoder = Openh264Decoder::default();

    let f = File::open(filename).await?;
    let mut reader = BufReader::new(f);

    let mut mp4_file = Mp4File::new_annexb(&mut reader);
    println!("streaming possible: {}", mp4_file.read_header().await?);

    let mut keys = mp4_file
        .tracks
        .iter()
        .filter(|&(_, v)| v.track_type() == TrackType::Video)
        .map(|(k, _)| *k);

    let track_id = keys.next().unwrap();
    let samples_len = mp4_file.tracks.get(&track_id).unwrap().samples.len();
    let codec = mp4_file.tracks.get(&track_id).unwrap().codec();
    let params = mp4_file.tracks.get(&track_id).unwrap().decode_params();

    let source = Arc::new(Mp4FrameSource {
        original: (),
        params: params.into_iter().collect(),
        codec,
        width: mp4_file.tracks.get(&track_id).unwrap().tkhd.width.value(),
        height: mp4_file.tracks.get(&track_id).unwrap().tkhd.height.value(),
    });

    for src in &source.params {
        decoder
            .push_data(src.clone(), 0, source.clone())
            .await
            .unwrap();
    }

    for idx in 0..samples_len {
        let samp = mp4_file.tracks.get(&track_id).unwrap().samples[idx].clone();

        let data = mp4_file.read_sample_data(track_id, idx).await?;

        let mut flags = FrameFlags::ENCODED | FrameFlags::VIDEO_STREAM;
        if samp.is_sync {
            flags.set(FrameFlags::KEYFRAME, true);
        }

        decoder
            .push_data(data.unwrap(), samp.start_time + samp.offset, source.clone())
            .await
            .unwrap();

        if let Some(decoded) = decoder.pull_frame().unwrap() {
            println!("{}x{}", decoded.width, decoded.height);
        }
    }

    Ok(())
}
