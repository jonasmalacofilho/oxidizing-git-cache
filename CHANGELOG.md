# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).


## [0.1.0] - TODO

<!--

### Added

- Add unit and integreation tests using a local server
- Add GitHub integration test

### Changed

- Drop Haxe and rewrite in Rust

### Deprecated

- Upstream URLs that don't specify the scheme (`http://` or `https://`)

### Fixed

- Protect credentials from leaking to disk and against command-line snooping of child git processes
  (git-cache-http-server#10)
- Batch concurrent requests for the same repository (git-cache-http-server#25)

-->


## [0.0.3] - TODO

Final Haxe/JS/Node release, for archival and documentation purposes only_

Users are strongly recommended to ignore this release and instead move to the new Rust
implementation (v0.1.0 or later).

The Haxe/JS/Node code published as v0.0.3 hasn't been maintained since 2020.


## [0.0.3-alpha] - Not released (last change: 2020-07-13)

Last functional change of the Haxe/JS/Node implementation.

### Added

- Add HTTP proxy support through `http_proxy` environment variable (PR git-cache-http-server#14,
  git-cache-http-server#4)

### Changed

- Log server and upstream operations (PR git-cache-http-server#9)
- Log repository and (sanitized) user
- Remove hmm and manage Haxe dependencies with haxelib alone
- Replace dummy executable with compile-time shebang insertion and permission adjustment
- Automate Haxe installation and project build through `package.json`
- Clean up output by replacing Haxe `trace()` with `println()`

### Fixed

- Recover from exceptions in callbacks (PR git-cache-http-server#3)
- Prevent multiple git processes from simultaneously operating on the same repository (PR
  git-cache-http-server#3)
- Update remote URL before fetching (PR git-cache-http-server#9)
- Increase request timeout limit to 120 minutes (PR git-cache-http-server#13)
- Fix compiler errors and warnings with Haxe >= 4.0.0
- Fix broken old http-proxy-agent on recent Node.js versions
- Prune objects that have been deleted in the remote (PR git-cache-http-server##22)


## [0.0.2] - 2017-06-06

### Changed

- Switch to hmm for dependency management on the Haxe side

### Fixed

- Add support for clients using gzip content encoding (PR git-cache-http-server#2,
  git-cache-http-server#1)


## [0.0.1-alpha] - 2015-11-23

### Added

- Add "smart" protocol `git-upload-pack` service (i.e. support `clone` and `fetch`)
- Add basic HTTP authentication
- Add `-p,--port` and `-c,--cache-dir` CLI options
- Add `--version` CLI option
