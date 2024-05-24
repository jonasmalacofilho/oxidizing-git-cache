# A caching Git HTTP server

Mirror remote repositories and serve them over HTTP, automatically updating
them as needed.

Currently supported client operations are fetch and clone.  Authentication to
the upstream repository is always enforced (for now, only HTTP Basic is
supported), but public repositories can be used as well.


## Usage

```
Usage:
  git-cache-http-server.js [options]

Options:
  -c,--cache-dir <path>   Location of the git cache [default: /var/cache/git]
  -p,--port <port>        Bind to port [default: 8080]
  -h,--help               Print this message
  --version               Print the current version
```

The upstream remote is extracted from the URL, taking the first component as
the remote hostname.

Example:

```
git-cache-http-server --port 1234 --cache-dir /tmp/cache/git &
git clone http://localhost:1234/github.com/jonasmalacofilho/git-cache-http-server
```

If you run your git-cache on a dedicated server or container (i.e. named
gitcache), you can then also configure git to always use your cache like in the
following example (don't use this configuration on the git-cache machine
itself):.

```
git config --global url."http://gitcache:1234/".insteadOf https://
```


## Installation

Requirements:

- Git command line tools (including `git-upload-pack`)

With the requirements installed, install git-cache-http-server from your package manager/registry of
choice:

- crates: `cargo install git-cache-http-server`

If your package manager/registry of choice isn't listed, continue for instruction on building from
source.


## Building from source

Requirements:

- Git command line tools (including `git-upload-pack` and `git-http-backend`)
- `cargo`, `rustc` (`rustup` is recommended)
- `lightttpd` (for local integration tests)

Clone the repository:

```
$ git clone https://github.com/jonasmalacofilho/git-cache-http-server
$ cd git-cache-http-server
```

Run the tests and build the server with optimizations.

```
$ cargo test
$ cargo build --release
```


## Example systemd service file

To install a cache service on Linux systems, check the example
`doc/git-cache-http-server.service` unit file.

For Systemd init users that file should not require major tweaks, other than
specifying a different than default port number or cache directory.  After
installed in the proper Systemd unit path for your distribution:

```
# systemctl daemon-reload
# systemctl start git-cache-http-server
# systemctl enable git-cache-http-server
```


## Dealing with problems

Environment variables available for the client:

- `GIT_TRACE`
- `GIT_TRACE2`
- `GIT_TRACE_PACKET`
- `GIT_TRACE_CURL`
- `GIT_TRACE_NO_DATA`
- `GIT_CURL_VERBOSE`

Additional environment variables available for the server:

- `RUST_LOG=<filter>`


## The "smart" Git HTTP protocol

The current implementation is somewhat oversimplified; any help in improving it
is greatly appreciated!

References:

 - [Transfer protocols on the Git Book](http://git-scm.com/book/en/v2/Git-Internals-Transfer-Protocols)
 - [Git documentation on the HTTP transfer protocols](https://github.com/git/git/blob/master/Documentation/technical/http-protocol.txt)
 - [Source code for the GitLab workhorse](https://gitlab.com/gitlab-org/gitlab-workhorse/blob/master/handlers.go)
 - [Source code for `git-http-backend`](https://github.com/git/git/blob/master/http-backend.c)
