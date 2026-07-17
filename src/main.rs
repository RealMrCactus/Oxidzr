use std::fs::OpenOptions;
use std::io::{self, BufRead, Seek, SeekFrom, Write};
use std::path::PathBuf;
use std::sync::Arc;
use std::thread;
use clap::Parser;
use rand::RngCore;

// ─── CLI ─────────────────────────────────────────────────────────────────────

#[derive(Parser, Debug)]
#[command(
    name = "maws",
    about = "Progressive high-entropy drive destruction tool",
    long_about = None
)]
struct Args {
    /// Target block device (e.g. /dev/sdc)
    #[arg(value_name = "DEVICE")]
    device: String,

    /// Write chunk size in MB (default: 16)
    #[arg(short, long, default_value_t = 16)]
    chunk_mb: usize,

    /// Number of random buffers in the pool (default: 8)
    #[arg(short, long, default_value_t = 8)]
    buffers: usize,

    /// Number of parallel write threads for SSD mode (default: 4)
    #[arg(short, long, default_value_t = 4)]
    threads: usize,

    /// Skip the confirmation prompt (for scripted or time-critical use)
    #[arg(short, long, default_value_t = false)]
    yes: bool,
}

// ─── sysfs macro ─────────────────────────────────────────────────────────────

macro_rules! read_sysfs {
    ($dev:expr, $file:expr) => {{
        let dev_str = $dev.to_string();

        // Whole disks and NVMe namespaces have their own dir under /sys/block;
        // partitions do not (they live under the parent disk's dir).
        let path = if std::path::Path::new(&format!("/sys/block/{}", dev_str)).exists() {
            // Whole disk: sdX, vdX, nvmeXnY, etc.
            format!("/sys/block/{}/{}", dev_str, $file)
        } else {
            // Partition. NVMe: nvme0n1p1 -> nvme0n1. SATA: sda1 -> sda.
            let parent = match dev_str.rfind('p') {
                Some(i) if dev_str.starts_with("nvme") => &dev_str[..i],
                _ => dev_str.trim_end_matches(|c: char| c.is_ascii_digit()),
            };
            // `queue/` attrs live on the parent drive; per-partition attrs
            // (e.g. `size`) live under the partition dir.
            if $file.starts_with("queue/") {
                format!("/sys/block/{}/{}", parent, $file)
            } else {
                format!("/sys/block/{}/{}/{}", parent, dev_str, $file)
            }
        };

        let content = std::fs::read_to_string(&path)
            .unwrap_or_else(|_| panic!("Failed to read sysfs path: {}", path));
        content
            .trim()
            .parse()
            .unwrap_or_else(|_| panic!("Failed to parse sysfs value at: {}", path))
    }};
}
// ─── Drive ───────────────────────────────────────────────────────────────────

#[derive(Clone, Debug)]
struct Drive {
    /// Full device path, e.g. /dev/sdc
    path: String,
    /// Kernel name used for sysfs lookups, e.g. sdc
    dev_name: String,
    /// Total size in bytes
    size: u64,
    rotational: bool,
}

impl Drive {
    fn from_path(raw: &str) -> Self {
        // Accept both "/dev/sdc" and "sdc"
        let path = if raw.starts_with("/dev/") {
            raw.to_string()
        } else {
            format!("/dev/{}", raw)
        };

        let dev_name = PathBuf::from(&path)
            .file_name()
            .expect("Invalid device path")
            .to_string_lossy()
            .to_string();

        let sectors: u64 = read_sysfs!(dev_name, "size");
        let rotational_raw: u8 = read_sysfs!(dev_name, "queue/rotational");

        Drive {
            path,
            dev_name,
            size: sectors * 512,
            rotational: rotational_raw != 0,
        }
    }
}

// ─── Buffer pool ─────────────────────────────────────────────────────────────

/// Allocate `count` buffers each of `chunk_mb` MB filled with random bytes.
fn build_pool(chunk_mb: usize, count: usize) -> Arc<Vec<Vec<u8>>> {
    let byte_len = chunk_mb * 1024 * 1024;
    let mut pool: Vec<Vec<u8>> = Vec::with_capacity(count);
    let mut rng = rand::rng();

    for i in 0..count {
        let mut buf = vec![0u8; byte_len];
        rng.fill_bytes(&mut buf);
        println!("  [pool] buffer {}/{} filled ({} MB)", i + 1, count, chunk_mb);
        pool.push(buf);
    }

    Arc::new(pool)
}

// ─── Decapitation ────────────────────────────────────────────────────────────

/// Destroy the partition table (offset 0) and the GPT backup header (end of drive).
/// Takes < 1 second; renders the drive immediately unmountable.
fn decapitate(target: &Drive, pool: &Arc<Vec<Vec<u8>>>) -> Result<(), std::io::Error> {
    println!("\n[decapitate] targeting {} ...", target.path);

    let mut file = OpenOptions::new()
        .write(true)
        .open(&target.path)?;

    let chunk_size = pool[0].len() as u64;
    let mut rng = rand::rng();

    // --- Head (MBR / GPT primary header) ---
    let idx = (rng.next_u64() as usize) % pool.len();
    file.seek(SeekFrom::Start(0))?;
    file.write_all(&pool[idx])?;
    println!("  [decapitate] head destroyed (offset 0)");

    // --- Tail (GPT backup header) ---
    if target.size > chunk_size {
        let end_offset = target.size - chunk_size;
        let idx = (rng.next_u64() as usize) % pool.len();
        file.seek(SeekFrom::Start(end_offset))?;
        file.write_all(&pool[idx])?;
        println!("  [decapitate] tail destroyed (offset {})", end_offset);
    }

    file.sync_all()?;
    println!("[decapitate] done — drive is unmountable.");
    Ok(())
}

// ─── Progressive wipe (HDD — single-threaded) ────────────────────────────────

/// Recursive-halving wipe for spinning disks.
/// Large sequential writes minimise head seek latency.
fn progressive_hdd(target: &Drive, pool: Arc<Vec<Vec<u8>>>) -> Result<(), std::io::Error> {
    println!("\n[progressive-hdd] starting on {} ...", target.path);

    let chunk_bytes = pool[0].len() as u64;
    let mut file = OpenOptions::new().write(true).open(&target.path)?;
    let mut rng = rand::rng();

    let mut stride = target.size / 2; // starting gap between writes
    let mut pass = 1usize;

    while stride >= chunk_bytes {
        let stride_mb = stride / (1024 * 1024);
        println!("  [hdd pass {}] stride = {} MB", pass, stride_mb);

        let mut pos = stride; // first write lands in the middle of the first gap
        let step = stride * 2;

        while pos + chunk_bytes <= target.size {
            let idx = (rng.next_u64() as usize) % pool.len();
            file.seek(SeekFrom::Start(pos))?;
            file.write_all(&pool[idx])?;
            pos += step;
        }

        file.sync_all()?;
        stride /= 2;
        pass += 1;
    }

    println!("[progressive-hdd] complete.");
    Ok(())
}

// ─── Progressive wipe (SSD/NVMe — multi-threaded) ────────────────────────────

/// Recursive-halving wipe for SSDs/NVMe.
/// Spawns `thread_count` workers per pass to saturate the controller queue.
fn progressive_ssd(
    target: &Drive,
    pool: Arc<Vec<Vec<u8>>>,
    thread_count: usize,
) -> Result<(), std::io::Error> {
    println!(
        "\n[progressive-ssd] starting on {} with {} threads ...",
        target.path, thread_count
    );

    let chunk_bytes = pool[0].len() as u64;
    let drive_size = target.size;
    let path = Arc::new(target.path.clone());

    let mut stride = drive_size / 2;
    let mut pass = 1usize;

    while stride >= chunk_bytes {
        let stride_mb = stride / (1024 * 1024);
        println!("  [ssd pass {}] stride = {} MB", pass, stride_mb);

        // Collect all write positions for this pass
        let step = stride * 2;
        let mut positions: Vec<u64> = Vec::new();
        let mut pos = stride;
        while pos + chunk_bytes <= drive_size {
            positions.push(pos);
            pos += step;
        }

        // Distribute positions across threads
        let chunks: Vec<Vec<u64>> = {
            let per_thread = (positions.len() + thread_count - 1) / thread_count;
            positions.chunks(per_thread).map(|c| c.to_vec()).collect()
        };

        let handles: Vec<_> = chunks
            .into_iter()
            .map(|pos_chunk| {
                let pool = Arc::clone(&pool);
                let path = Arc::clone(&path);

                thread::spawn(move || -> Result<(), std::io::Error> {
                    let mut file = OpenOptions::new().write(true).open(path.as_str())?;
                    let mut rng = rand::rng();

                    for p in pos_chunk {
                        let idx = (rng.next_u64() as usize) % pool.len();
                        file.seek(SeekFrom::Start(p))?;
                        file.write_all(&pool[idx])?;
                    }
                    file.sync_all()?;
                    Ok(())
                })
            })
            .collect();

        for handle in handles {
            handle
                .join()
                .expect("Worker thread panicked")?;
        }

        stride /= 2;
        pass += 1;
    }

    println!("[progressive-ssd] complete.");
    Ok(())
}

// ─── Entry point ─────────────────────────────────────────────────────────────

fn main() {
    let args = Args::parse();

    println!("════════════════════════════════════");
    println!(" maws — progressive drive destructor");
    println!("════════════════════════════════════");

    // Resolve drive info
    let drive = Drive::from_path(&args.device);
    println!(
        "\nTarget : {}\nSize   : {} GiB\nType   : {}",
        drive.path,
        drive.size / 1024 / 1024 / 1024,
        if drive.rotational { "HDD (single-threaded mode)" } else { "SSD/NVMe (multi-threaded mode)" }
    );

    // Confirmation prompt (skipped with -y)
    if !args.yes {
        println!(
            "\n  !! WARNING: This will permanently destroy all data on {} !!\n",
            drive.path
        );
        print!("  Type the device path to confirm, or anything else to abort: ");
        io::stdout().flush().expect("Failed to flush stdout");

        let stdin = io::stdin();
        let input = stdin
            .lock()
            .lines()
            .next()
            .expect("Failed to read input")
            .expect("Failed to read line");

        if input.trim() != drive.path {
            eprintln!("\n  Aborted — input did not match. No data was written.");
            std::process::exit(1);
        }
        println!();
    }

    // Build buffer pool
    println!("[pool] allocating {} × {} MB buffers ...", args.buffers, args.chunk_mb);
    let pool = build_pool(args.chunk_mb, args.buffers);

    // Phase 1: decapitation
    decapitate(&drive, &pool).expect("Decapitation failed");

    // Phase 2: progressive wipe
    if drive.rotational {
        progressive_hdd(&drive, Arc::clone(&pool)).expect("Progressive HDD wipe failed");
    } else {
        progressive_ssd(&drive, Arc::clone(&pool), args.threads)
            .expect("Progressive SSD wipe failed");
    }

    println!("\n[maws] destruction complete on {}.", drive.path);
}
