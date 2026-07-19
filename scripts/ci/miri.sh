#!/usr/bin/env bash
# SSOT: Miri 门禁（CI / Release 共用，避免 skip 列表漂移）
# dir_sync 与平台 IO 同列为 isolation skip（CI/Release 必须同源）
set -euo pipefail

export RUSTUP_TOOLCHAIN="${RUSTUP_TOOLCHAIN:-nightly}"

cargo miri setup

cargo miri test -p tachyon-core --lib -- \
  --skip test_validate_save_path \
  --skip test_validate_multi_save_paths \
  --skip proptests

# dir_sync 与平台 IO 同列为 isolation skip（CI/Release 必须同源）
cargo miri test -p tachyon-io --lib -- \
  --skip iocp --skip iouring --skip pipeline \
  --skip tokio_file --skip winio --skip write_pipeline \
  --skip dir_sync
