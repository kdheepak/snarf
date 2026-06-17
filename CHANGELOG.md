# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.2.0](https://github.com/kdheepak/snarf/compare/v0.1.1...v0.2.0) - 2026-06-17

### Added

- [**breaking**] remove docs subcommand

### Other

- *(readme)* remove sections on backends, browser support, and cache
- *(readme)* remove section on showing effective config and backends
- *(readme)* remove description of CLI features and caching
- *(snarf_cli)* add unix cfg attribute to wait_for_flag and wait_for_crawl_status
- *(config)* add unix cfg guards to imports and static in tests
- *(readme)* update usage examples and config instructions
- *(core)* remove fs_atomic module and inline atomic write logic
