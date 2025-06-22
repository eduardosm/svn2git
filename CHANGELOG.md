# Changelog

## 0.3.0 (2025-06-22)

### Breaking

- Unbranched branch will not be created if `unbranched` is not specified in the
  conversion parameters file.

### Fixed

- Fix incorrectly generated Git deltas for objects larger than 16777215 bytes
  (2^24 - 1).
- Fix panic when merging the creation commit of an unrelated branch.

### Other

- MSRV has been bumped to 1.82.

## 0.2.1 (2024-09-09)

### Changed

- Improved error message on failure to open the Subversion source.

## 0.2.0 (2024-08-10)

### Breaking

- Conversion parameters format is now TOML instead of YAML.

## 0.1.0 (2024-07-22)

- Initial release
