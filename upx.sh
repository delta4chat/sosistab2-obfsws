#!/bin/bash

# a warpper for "post-run" build scripts
# see also https://github.com/rust-lang/cargo/issues/545

command $*
status_code="$?"
find ./target/ \( -name wsocks -or -name wsocks.exe \) -exec upx '{}' \;
exit "$status_code"

