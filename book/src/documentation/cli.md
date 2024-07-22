# Command Line Arguments

* `--stderr-log-level <LEVEL>` (default: `warn`)

  Maximum level to log on stderr. Possible values are `error`, `warn`, `info`,
  `debug`, `trace`.

* `--log-file <PATH>`

  File where to save logs (in addition to stderr). If the file does not exist,
  it will be created. Otherwise, new logs will be appended at the end.

* `--file-log-level <LEVEL>` (default: `debug`)

  Maximum level to log on the file specified by `--log-file`. Possible values
  are `error`, `warn`, `info`, `debug`, `trace`.

* `--no-progress`

  Disable progress output.

* `-s <PATH>` or `--src <PATH>` (required)

  Path to the source Subversion repository. It can be:

  * A Subversion dump file, version 2 (without deltas) or 3 (with deltas), and
    optionally compressed with gzip, bzip2, XZ, zstd, or LZ4.
  * A local Subversion repository (i.e., the directory that is managed with
    `svnadmin`). In this case, `svnadmin dump` will be executed automatically
    and its output is consumed on the fly.

* `-d <PATH>` or `--dest <PATH>` (required)

  Destination where the new Git repository will be created. A bare repository
  will be created at this location.

* `-P <FILE>` or `--conv-params <FILE>` (required)

  Path to a file in YAML format used to configure the conversion. See the
  [Conversion Parameters](./conv-params.md) section.

* `--obj-cache-size <SIZE>` (default: `384`)

  Changes the size (in MiB) of the in-memory Git object cache. Do not use this
  option unless you know what you are doing.

* `--git-repack`

  Runs `git repack` at the end of the conversion. It may cause the repository
  to grow or shrink.
