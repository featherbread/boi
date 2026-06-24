# boi

> _breath in_... **backup**

boi is a small, opinionated wrapper for [Borg](https://www.borgbackup.org/) that:

  * Creates new backup archives with consistent settings
  * Uses APFS snapshots on macOS to avoid file drift during a backup
  * Prunes archives using one of a handful of simple retention policies

It's designed for my use alone, as a Rust rewrite of wrapper scripts that I
wrote for these purposes in the past instead of adopting any more widely
accepted Borg automation solution. It's made public in the hope that the code
might be useful as a reference (probably for myself more than anyone else).
