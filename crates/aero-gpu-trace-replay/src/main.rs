use std::env;
use std::fs::File;
use std::io::BufReader;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = env::args().skip(1);
    let Some(path) = args.next() else {
        eprintln!("usage: aero-gpu-trace-replay <trace.aerogputrace>");
        std::process::exit(2);
    };

    let file = File::open(&path)?;
    let frames = aero_gpu_trace_replay::replay_trace(BufReader::new(file))?;
    for frame in frames {
        println!(
            "frame {}: {}x{} sha256={}",
            frame.frame_index,
            frame.width,
            frame.height,
            frame.sha256()
        );
    }
    Ok(())
}

