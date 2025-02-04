use glob::glob;
use mp4::TrackType;
use tokio::fs::File;
use tokio::io::BufReader;

#[tokio::test]
async fn test_read_mp4() {
    let paths = glob("./assets/videos/*.mp4").expect("Failed to read glob pattern");

    for path in paths {
        if let Ok(path) = path {
            println!("\n{}", path.display());
            let f = File::open(path).await.unwrap();
            let mut reader = BufReader::new(f);

            let mut mp4_file = mp4::Mp4File::new(&mut reader);
            println!(
                "streaming possible: {}",
                mp4_file.read_header().await.unwrap()
            );

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
                    .await
                    .unwrap()
                    .map(|x| x.slice(0..10));

                // println!(
                //     "[{} {} {}] {} - <{}> {} +{} {:?}",
                //     idx + 1,
                //     samp.chunk_id,
                //     samp.offset,
                //     samp.is_sync,
                //     samp.size,
                //     samp.start_time,
                //     samp.rendering_offset,
                //     data.as_deref()
                // );
            }
        }
    }
}
