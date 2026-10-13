#!/bin/sh
# Copyright © SixtyFPS GmbH <info@slint.dev>
# SPDX-License-Identifier: GPL-3.0-only OR LicenseRef-Slint-Royalty-free-2.0 OR LicenseRef-Slint-Software-3.0
set -eu

corner_demo_dir=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
corner_repo_dir=$(CDPATH= cd -- "$corner_demo_dir/../.." && pwd)

export SLINT_BACKEND=winit-skia
exec cargo run --release --locked --manifest-path "$corner_repo_dir/Cargo.toml" \
    -p slint-viewer --no-default-features --features backend-winit,renderer-skia \
    -- "$@" "$corner_demo_dir/main.slint"
