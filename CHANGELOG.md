# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.2.1](https://github.com/kdheepak/snarf/compare/v0.2.0...v0.2.1) - 2026-06-29

### Added

- *(crawl)* improve sitemap fetching with multiple candidates and error handling

### Other

- *(deps)* bump actions/checkout from 6 to 7 ([#9](https://github.com/kdheepak/snarf/pull/9))
- *(workflows)* update GITHUB_TOKEN usage in release-plz workflow
- *(tests)* fix formatting in snarf_cli.rs
- *(tests)* fix formatting in cancel_signal.rs
- *(updatecheck)* fix formatting of match arms and remove trailing commas
- *(main)* remove trailing commas in match arms and blocks
- *(crawl)* fix trailing commas in match arms and blocks
- *(config)* fix formatting of match arms in github_token_from_gh
- *(snarf_cli)* add trailing commas to match formatting conventions
- *(cancel_signal)* add trailing commas to match formatting conventions
- *(updatecheck)* fix formatting of match arms and trailing commas
- *(main)* add trailing commas to match Rust formatting conventions
- *(config)* fix trailing comma formatting in github_token_from_gh
- *(crawl)* add tests for sitemap fetching and HTML error handling

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
