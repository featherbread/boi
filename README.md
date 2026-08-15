# boi

> _breath in_... **backup**

```
[boi] ✓ Created and mounted APFS snapshot 2026-07-11-120102.
[boi] ✓ Archived 18.35 GiB in 267561 files.
      ┌ nas ✓ 267561 N 18.35 GiB S 11.62 GiB C 22.74 MiB D
      └ Created archive in 106.872165 seconds
      ┌ remote ✓ 267561 N 18.35 GiB S 11.62 GiB C 26.27 MiB D
      └ Created archive in 107.827067 seconds
[boi] ✓ Unmounted APFS snapshot; deleting in background.
```

boi is a small, opinionated wrapper for [Borg](https://www.borgbackup.org/) that:

  * Backs up `$HOME` to multiple repos concurrently
  * Uses APFS snapshots on macOS to avoid file drift
  * Prunes archives on request using simple retention policies

It's something I've designed entirely for myself and my usage patterns, as an
evolution of Bash scripts that I preferred (for whatever reason) to any more
widely known Borg automation solution. It's made public in the hope that the
code might be useful as a reference for my future self.
