#!/bin/bash

# a warpper for "post-run" build scripts
# see also https://github.com/rust-lang/cargo/issues/545

command $*
status_code="$?"
upx "$(find ./target/ -name wsocks -or -name wsocks.exe)"
exit "$status_code"

