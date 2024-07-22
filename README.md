# svn2git

A Subversion to Git repository converter

## Features

* Uses Subversion dump files as input.
  * Supports version 2 (without deltas) and version 3 (with deltas).
  * It can be optionally compressed with gzip, bzip2, XZ, zstd or LZ4.
* It does not require to have Git installed in the same machine.
* Efficient. Given the [old GCC Subversion repository](svn://gcc.gnu.org/svn/gcc)
  (280157 revisions), provided as a version 3 (with deltas) dump compressed with
  XZ:
  * It takes 50 minutes to finish with an Intel i7-8750H CPU and a SATA SSD.
  * It uses up to 2.2 GiB of RAM and 6.5 GiB of disk during conversion.
  * The resulting Git repository takes 2.8 GiB of disk, without needing to run
    `git repack`.

## Install

### Dependencies

svn2git does not have runtime dependencies, but you will likely need Subversion
(to prepare the origin repository) and Git (to do anything with the result of
the conversion).

### Build from source

If you have a Rust toolchain installed, you can clone this repository and build
a working executable:

```sh
cargo build --release
```

## Usage

Check the documentation and tutorial from the [book](./book/src/SUMMARY.md).

## License

Licensed under either of

* Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE) or
  <https://www.apache.org/licenses/LICENSE-2.0>)
* MIT license ([LICENSE-MIT](LICENSE-MIT) or
  <https://opensource.org/licenses/MIT>)

at your option.
