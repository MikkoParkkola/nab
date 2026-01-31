# Changelog

All notable changes to nab will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased] - 2026-01-25

### Fixed
- `stream --duration` flag now works for file output (was only working for player piping)
- `analyze` command now properly detects audio-only files and skips video frame extraction

### Changed
- Native HLS backend respects duration limit via segment counting
- FFmpeg backend passes duration via `-t` flag
