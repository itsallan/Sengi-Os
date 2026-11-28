#!/bin/sh
# Builds a Flatpak repository for the tests in cancel_remove.rs: a small
# runtime made of a program of this machine, and apps that use it. It needs
# flatpak and ostree, and downloads nothing.
#
#   make-repo.sh DIRECTORY
#
# The repository is DIRECTORY/repo.
set -eu

[ $# -eq 1 ] || {
	echo "usage: make-repo.sh DIRECTORY" >&2
	exit 2
}
out=$1
arch=$(flatpak --default-arch)
platform=org.sengi.test.Platform
rm -rf "$out/repo" "$out/build"
mkdir -p "$out/build"

# Data that does not compress, in one big file and many small ones, so that
# both the download and the deployment take long enough to be interrupted.
filler() {
	mkdir -p "$1/many"
	head -c "$2" /dev/urandom >"$1/blob"
	i=0
	while [ $i -lt "$3" ]; do
		echo "$1 $i" >"$1/many/$i"
		i=$((i + 1))
	done
}

runtime=$out/build/runtime
mkdir -p "$runtime/files/bin" "$runtime/usr/bin"
cp /usr/bin/sleep "$runtime/usr/bin/"
# The libraries and the loader it needs, where the sandbox looks for them.
ldd /usr/bin/sleep | grep -o '/[^ ]*' | while read -r library; do
	mkdir -p "$runtime/usr${library%/*}"
	cp -L "$library" "$runtime/usr$library"
done
filler "$runtime/usr/share/filler" 12000000 1500
cat >"$runtime/metadata" <<EOF
[Runtime]
name=$platform
runtime=$platform/$arch/stable
sdk=$platform/$arch/stable
EOF
flatpak build-export --runtime --disable-sandbox "$out/repo" "$runtime" stable >/dev/null

# Branched is the one app with two branches, so that installing the others
# by their ID alone stays a question with one answer.
for app in Hello Other Branched; do
	id=org.sengi.test.$app
	dir=$out/build/$app
	mkdir -p "$dir/files/bin" "$dir/export/share/applications"
	cp /usr/bin/sleep "$dir/files/bin/hello"
	filler "$dir/files/share/filler" 24000000 1500
	cat >"$dir/metadata" <<EOF
[Application]
name=$id
runtime=$platform/$arch/stable
sdk=$platform/$arch/stable
command=hello
EOF
	cat >"$dir/export/share/applications/$id.desktop" <<EOF
[Desktop Entry]
Type=Application
Name=$app
Exec=hello
EOF
	flatpak build-export --disable-sandbox "$out/repo" "$dir" stable >/dev/null
	if [ "$app" = Branched ]; then
		flatpak build-export --disable-sandbox "$out/repo" "$dir" beta >/dev/null
	fi
done
flatpak build-update-repo "$out/repo" >/dev/null
rm -rf "$out/build"
echo "$out/repo"
