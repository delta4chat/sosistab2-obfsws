#!/bin/bash

# a warpper for "post-run" build scripts
# see also https://github.com/rust-lang/cargo/issues/545

tmp=$(mktemp || exit)

tee $tmp <<"EOF"
if type upx
then
	upx $1 && exit
fi

exit

if type xz
then
	cat $1 | xz -c -v -e -9 > $1.xz && exit
fi

if type gzip
then
	cat $1 | gzip -c -9 > $1.gz && exit
fi

echo failed to compress file size, fallback to strip?
EOF

command $*
status_code="$?"
find ./target/ \( -name wsocks -or -name wsocks \) -exec sh $tmp '{}' \;
exit "$status_code"

