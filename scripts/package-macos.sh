#!/bin/sh
# Build the v0 macOS artifact from the pinned lockfile and assemble the
# documented reader/author bundle. Run from anywhere; output lands in dist/.
set -eu

root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
cd "$root"

arch=$(uname -m)
case "$arch" in
arm64 | aarch64) arch=aarch64 ;;
x86_64) arch=x86_64 ;;
*)
	echo "glisk: unsupported macOS architecture: $arch" >&2
	exit 1
	;;
esac

name="glisk-v0-macos-$arch"
out="dist/$name"

cargo build --release --locked --manifest-path spike/Cargo.toml
mkdir -p "$out/docs" "$out/examples"
install -m 0755 spike/target/release/glisk "$out/glisk"
install -m 0755 spike/target/release/glisk dist/glisk
install -m 0644 examples/decision-report.html "$out/examples/decision-report.html"
install -m 0644 docs/authoring.md "$out/docs/authoring.md"
install -m 0644 docs/cli.md "$out/docs/cli.md"

cat >"$out/README.txt" <<'EOF'
Glisk v0 - live local HTML report pager for macOS/Ghostty 1.4+

Run:
  ./glisk examples/decision-report.html
  ./glisk examples/decision-report.html --text
  cat examples/decision-report.html | ./glisk -

Read docs/authoring.md before generating a new report. Glisk is a local
document reader: one .htm/.html file or stdin, inline CSS/SVG/PNG only, no
scripts, network, secondary files, or navigation away from the document.
Ghostty 1.4+ is the verified graphics target. Other terminals fall back to
plain text when the Kitty probe or cell geometry is unavailable.

The packaged binary is unsigned. When it is downloaded rather than built
locally, clear the quarantine flag first:
  xattr -d com.apple.quarantine ./glisk

Verify the bundle:
  cd glisk-v0-macos-<arch> && shasum -a 256 -c SHA256SUMS
EOF

# Checksums are relative to the bundle root.
(
	cd "$out"
	find . -type f ! -name SHA256SUMS | LC_ALL=C sort | xargs shasum -a 256 >SHA256SUMS
)

# One deterministic tarball: fixed file metadata and no gzip timestamp.
rm -f "dist/$name.tar" "dist/$name.tar.gz"
TZ=UTC find "$out" -exec touch -t 202601010000 {} +
TZ=UTC tar --format=ustar --uid 0 --gid 0 --uname root --gname wheel \
	-cf "dist/$name.tar" -C dist "$name"
gzip -9n -c "dist/$name.tar" >"dist/$name.tar.gz"
rm -f "dist/$name.tar"

shasum -a 256 dist/glisk "$out/glisk" "dist/$name.tar.gz" >"dist/$name.sha256"

echo "glisk: binary   $root/dist/glisk"
echo "glisk: bundle   $root/dist/$name.tar.gz"
echo "glisk: checksum $root/dist/$name.sha256"
cat "dist/$name.sha256"
