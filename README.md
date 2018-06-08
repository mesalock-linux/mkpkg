mkpkg
=====

[![Build Status](https://ci.mesalock-linux.org/api/badges/mesalock-linux/mkpkg/status.svg?branch=master)](https://ci.mesalock-linux.org/mesalock-linux/mkpkg)
[![License](https://img.shields.io/badge/license-BSD-blue.svg)](LICENSE)
[![LoC](https://tokei.rs/b1/github/mesalock-linux/mkpkg)](https://github.com/mesalock-linux/mkpkg)

A parallel package builder for MesaLock Linux (inspired by Arch Linux's
`makepkg`).

Features
--------

* Build scripts written in YAML
    * Commands are executed using `sh`
* Download and build multiple packages at the same time
* Log all build output for later review
* Automatically extract compressed/archived files (_e.g._ `.tar.gz`, `.tar.xz`)
* Download using Git (through [libgit2][]) and HTTP/HTTPS (using [reqwest][])
* Display progress using multiple progress bars

Maintainer
----------

* Alex Lyon `<alexlyon@baidu.com>` [@Arcterus](https://github.com/Arcterus)
* Mingshen Sun `<mssun@mesalock-linux.org>` [@mssun](https://github.com/mssun)

License
-------

`mkpkg` is provided under the 3-Clause BSD license (please see LICENSE for more
details).

[libgit2]: https://crates.io/crates/git2
[reqwest]: https://crates.io/crates/reqwest
