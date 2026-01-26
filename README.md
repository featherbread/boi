# boi

> _breath in_... **backup**

boi is a small, opinionated wrapper for [Borg](https://www.borgbackup.org/) that:

  * Creates new backup archives with consistent settings
  * Uses APFS snapshots on macOS to avoid file drift during a backup
  * Prunes archives using one of a handful of simple retention policies
  * Optionally supports the anti-pattern of "uploading" a complete repository
    to remote storage, as a stopgap until it can better work concurrently with
    multiple independent repositories

It's designed for my use alone, as a Rust rewrite of some wrapper scripts that
I used for these purposes in the past, but is made public in the hope that the
code might be useful as a reference (for myself more than anyone else).
