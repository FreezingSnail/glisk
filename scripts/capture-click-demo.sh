#!/bin/sh
# Rebuild docs/assets/glisk-click.gif from the live pager.
#
# The capture harness runs the real binary over a 177 x 55 PTY, sends SGR
# left-press reports at fixed cells, and dumps the live frame PNGs plus the
# exact status rows. This script composes each frame with a status row drawn in
# the recorded terminal font and encodes the GIF. Frames land in
# target/capture-click and are not committed.
#
#   scripts/capture-click-demo.sh
#   GLISK_CAPTURE_FONT=/path/to/FiraMonoNerdFontMono-Regular.otf \
#     scripts/capture-click-demo.sh
set -eu

root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
cd "$root"

out=${1:-target/capture-click}
font=${GLISK_CAPTURE_FONT:-"$HOME/Library/Fonts/FiraMonoNerdFontMono-Regular.otf"}
if [ ! -f "$font" ]; then
	echo "capture: set GLISK_CAPTURE_FONT to FiraMonoNerdFontMono-Regular.otf" >&2
	exit 1
fi

rm -rf "$out"
GLISK_CAPTURE_DIR="$root/$out" \
	cargo test --quiet --test cli_e2e capture_click_demo_frames -- --ignored

swiftc -O scripts/render-status-row.swift -o "$out/render-status-row"

# Stage order is fixed by the harness: report open, fragment click, summary click.
awk '/^status=/{ status = substr($0, 8) }
     /^frame=/{ print substr($0, 7) "\t" status }' \
	"$out/capture.txt" >"$out/stages.tsv"

: >"$out/concat.txt"
i=0
while IFS="$(printf '\t')" read -r frame status; do
	i=$((i + 1))
	printf '%s\n' "$status" >"$out/status-$i.txt"
	"$out/render-status-row" "$out/status-$i.txt" "$font" \
		3009 17 34 '#064957' '#f4fdff' "$out/row-$i.png"
	ffmpeg -v error -i "$frame" -i "$out/row-$i.png" \
		-filter_complex vstack=inputs=2 -y "$out/still-$i.png"
done <"$out/stages.tsv"

# Holds keep each state readable. The final state is listed twice because the
# concat demuxer only honors the last duration when another file follows.
cat >>"$out/concat.txt" <<EOF
file '$root/$out/still-1.png'
duration 1.2
file '$root/$out/still-2.png'
duration 1.6
file '$root/$out/still-3.png'
duration 1.2
file '$root/$out/still-3.png'
duration 1.4
EOF

ffmpeg -v error -f concat -safe 0 -i "$out/concat.txt" \
	-vf "fps=10,scale=1400:-1:flags=lanczos,split[a][b];[a]palettegen[p];[b][p]paletteuse" \
	-loop 0 -y docs/assets/glisk-click.gif

echo "capture: wrote docs/assets/glisk-click.gif ($(wc -c <docs/assets/glisk-click.gif | tr -d ' ') bytes)"
