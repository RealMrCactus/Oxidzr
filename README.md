> **⚠️ WARNING:** Maws is a destructive tool.
# Maws

**A progressive data destruction tool for high-entropy drive wiping in time-critical scenarios.**

> **⚠️ WARNING:** Maws is a destructive tool. It is designed to permanently destroy data. There is no undo, no recovery, and no "Are you sure?" safety net once execution begins. Use with extreme caution.

## 📖 What is Maws?

Maws is a specialized alternative to standard tools like `shred`. While `shred` wipes data linearly (Sector 1, Sector 2, Sector 3...), Maws uses a **Recursive Halving Algorithm** to destroy data structurally across the entire span of the drive simultaneously.

It is designed for scenarios where the time available to wipe a drive is undefined or potentially extremely short.

### The Problem with Linear Wiping

If you have a 1TB drive and only 5 minutes to wipe it before power is lost:

  * **Linear Wipe (`shred`):** You destroy the first 5% of the drive. The remaining 95% is perfectly intact and recoverable.
  * **Maws:** You destroy 5% of the drive, but that damage is distributed evenly as millions of "bullet holes" across the entire disk surface.

## ⚙️ How It Works

Maws employs a **"Swiss Cheese" strategy** to maximize file corruption speed.

1.  **Decapitation (0-1 Seconds):**
    Immediately overwrites the partition table (MBR/GPT) and backup headers at the end of the drive. The drive is instantly unmountable.

2.  **Recursive Halving (The Loop):**
    Instead of writing sequentially, Maws writes chunks of high-entropy random data at diminishing intervals. It creates a coarse grid of destruction that gets finer with every pass:

      * **Pass 1:** Writes a block every **set stride** (e.g., every 256MB).
      * **Pass 2:** Halves the stride (128MB). Writes a block in the *middle* of the previous gaps.
      * **Pass 3:** Halves the stride again (64MB).
      * **Pass N:** Continues until the gaps are smaller than the write block size (100% destruction).

**The Result:**

  * **@ 30 Seconds:** Large files (OS kernels, databases, videos) are fragmented and corrupt.
  * **@ 5 Minutes:** Medium files (Photos, Documents) are statistically likely to be hit.
  * **@ Completion:** The drive is 100% overwritten, identical to a standard wipe.

## 🚀 Features

  * **Progressive Destruction:** Damage is spread evenly; stopping the process early still results in catastrophic data loss across the whole file system.
  * **Smart Hardware Detection:**
      * **HDD Mode:** Uses single-threaded, large-block writes (16MB) to minimize physical seek latency and maximize throughput.
      * **SSD/NVMe Mode:** Uses multi-threaded, parallel writes to saturate the high-speed controller queues.
  * **High Entropy:** Uses cryptographically secure random data rather than zeros, preventing controller compression hacks.
  * **Atomic Syncing:** Forces physical disk flushes to ensure data is actually burned to the platter/NAND, not just sitting in RAM.

## 📦 Installation & Usage

**Requirements:**

  * Linux (Root privileges required)

**Build from Source:**

```bash
cargo build --release
```

**Usage:**

```bash
# Basic usage (Auto-detects HDD/SSD)
sudo ./maws /dev/sdX

# View help
sudo ./maws --help
```

## ⚖️ License

AGPLv3 - Open Source.
