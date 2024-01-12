#!/bin/bash

# a warpper for "post-run" build scripts
# see also https://github.com/rust-lang/cargo/issues/545

command $*
status_code="$?"
upx ./target/release/wsocks
exit "$status_code"

