#!/bin/sh
# Install Nmerkar's nk and nks commands on Linux or macOS.
# Usage: curl -fsSL https://raw.githubusercontent.com/curvedinf/Nmerkar/main/install.sh | sh
set -eu

repo_url=https://github.com/curvedinf/Nmerkar
min_rust_version=1.70.0
if [ "${NK_INSTALL_DIR+x}" = x ]; then
  install_dir=$NK_INSTALL_DIR
else
  # curl | sh cannot change the parent shell's PATH. Prefer an existing entry.
  case ":$PATH:" in
    *":$HOME/.local/bin:"*) install_dir=$HOME/.local/bin ;;
    *":/usr/local/bin:"*) install_dir=/usr/local/bin ;;
    *) install_dir=$HOME/.local/bin ;;
  esac
fi
source_ref=${NK_INSTALL_REF:-main}
archive_override=${NK_INSTALL_ARCHIVE:-}
source_override=${NK_INSTALL_SOURCE_DIR:-}
tmp_dir=$(mktemp -d)
trap 'rm -rf "$tmp_dir"' EXIT HUP INT TERM

say() { printf '%s\n' "nmerkar: $*"; }
fail() { say "error: $*" >&2; exit 1; }
as_root() {
  if [ "$(id -u)" -eq 0 ]; then "$@";
  elif command -v sudo >/dev/null 2>&1; then sudo "$@";
  else fail "root or sudo is required to run: $*"; fi
}
fetch() {
  if command -v curl >/dev/null 2>&1; then curl -fL --retry 2 --silent --show-error "$1" -o "$2";
  elif command -v wget >/dev/null 2>&1; then wget -q "$1" -O "$2";
  else fail 'curl or wget is required to download Nmerkar'; fi
}

os=$(uname -s)
arch=$(uname -m)
case "$os:$arch" in
  Linux:x86_64|Linux:amd64) platform=linux-x86_64-musl ;;
  Linux:aarch64|Linux:arm64) platform=linux-arm64-musl ;;
  Linux:armv7l|Linux:armv7) platform=linux-armv7 ;;
  Darwin:arm64) platform=macos-arm64 ;;
  Darwin:x86_64) platform=macos-x86_64 ;;
  *) fail "unsupported platform $os/$arch (supported: Linux x86_64, ARM64, ARMv7; macOS ARM64, x86_64)" ;;
esac

check_cc() {
  command -v cc >/dev/null 2>&1 || return 1
  printf '%s\n' '#include <pthread.h>' '#include <math.h>' '#include <sys/mman.h>' \
    '#include <stdatomic.h>' 'static _Thread_local int local;' \
    'int main(void) { _Atomic int n = 0; pthread_mutex_t m = PTHREAD_MUTEX_INITIALIZER; local = atomic_fetch_add(&n, 1); return pthread_mutex_lock(&m) || pthread_mutex_unlock(&m) || (sin(0.0) != 0.0) || local; }' > "$tmp_dir/probe.c"
  cc -O2 -o "$tmp_dir/probe" "$tmp_dir/probe.c" -lpthread -lm >/dev/null 2>&1 && "$tmp_dir/probe"
}

check_rust() {
  command -v cargo >/dev/null 2>&1 && command -v rustc >/dev/null 2>&1 || return 1
  version=$(rustc --version 2>/dev/null | awk '{print $2}')
  case "$version" in
    *.*) major=${version%%.*}; minor=${version#*.}; minor=${minor%%.*} ;;
    *) return 1 ;;
  esac
  case "$major:$minor" in *[!0-9:]*|'') return 1 ;; esac
  [ "$major" -gt 1 ] || { [ "$major" -eq 1 ] && [ "$minor" -ge 70 ]; }
}

install_cc() {
  case "$os" in
    Darwin)
      say 'Installing Apple Command Line Tools (complete the system prompt, then rerun this installer).'
      xcode-select --install || true
      fail 'the Apple Command Line Tools must finish installing before Nmerkar can run'
      ;;
    Linux)
      if command -v apt-get >/dev/null 2>&1; then
        as_root apt-get update
        as_root env DEBIAN_FRONTEND=noninteractive apt-get install -y --no-install-recommends build-essential ca-certificates tar curl
      elif command -v dnf >/dev/null 2>&1; then
        as_root dnf install -y gcc glibc-devel binutils ca-certificates tar curl
      elif command -v yum >/dev/null 2>&1; then
        as_root yum install -y gcc glibc-devel binutils ca-certificates tar curl
      elif command -v apk >/dev/null 2>&1; then
        as_root apk add --no-cache build-base ca-certificates tar curl
      elif command -v pacman >/dev/null 2>&1; then
        as_root pacman -Sy --noconfirm --needed base-devel ca-certificates tar curl
      elif command -v zypper >/dev/null 2>&1; then
        as_root zypper --non-interactive install gcc glibc-devel binutils ca-certificates tar curl
      else
        fail 'no supported package manager found; install a C compiler, libc headers, pthreads, and libm'
      fi
      ;;
  esac
}

if ! check_cc || ! command -v tar >/dev/null 2>&1; then
  say 'Installing the C compiler, development libraries, and archive tools.'
  install_cc
  check_cc || fail 'cc cannot compile and run a program with pthreads, libm, and mmap headers'
  command -v tar >/dev/null 2>&1 || fail 'tar is unavailable after dependency installation'
fi

case "$source_ref" in
  *[!a-zA-Z0-9._/-]*|'') fail 'NK_INSTALL_REF contains invalid characters' ;;
esac

if [ -n "$archive_override" ]; then
  [ -f "$archive_override" ] || fail "archive not found: $archive_override"
  cp "$archive_override" "$tmp_dir/nmerkar.tar.gz"
  mode=archive
elif [ -n "$source_override" ]; then
  [ -f "$source_override/comp/Cargo.toml" ] || fail "invalid source directory: $source_override"
  mode=source
else
  # Releases contain prebuilt nk+nks. Until the first release, build from main.
  release_url="$repo_url/releases/latest/download/nmerkar-$platform.tar.gz"
  if fetch "$release_url" "$tmp_dir/nmerkar.tar.gz" 2>/dev/null; then
    mode=archive
  else
    say 'No prebuilt release available; building from source.'
    mode=source
  fi
fi

if [ "$mode" = archive ]; then
  mkdir "$tmp_dir/package"
  tar --no-same-owner -xzf "$tmp_dir/nmerkar.tar.gz" -C "$tmp_dir/package" || fail 'invalid release archive'
  [ -f "$tmp_dir/package/nk" ] && [ -f "$tmp_dir/package/nks" ] || fail 'archive must contain both nk and nks'
  bin_dir="$tmp_dir/package"
else
  if [ -n "$source_override" ]; then
    source_dir=$source_override
  else
    fetch "$repo_url/archive/refs/heads/$source_ref.tar.gz" "$tmp_dir/source.tar.gz" || fail "could not fetch source ref $source_ref"
    mkdir "$tmp_dir/source"
    tar --no-same-owner -xzf "$tmp_dir/source.tar.gz" -C "$tmp_dir/source" --strip-components=1 || fail 'invalid source archive'
    source_dir=$tmp_dir/source
  fi
  if ! check_rust; then
    say "Installing Rust $min_rust_version to build Nmerkar from source."
    fetch https://sh.rustup.rs "$tmp_dir/rustup.sh" || fail 'could not download rustup'
    sh "$tmp_dir/rustup.sh" -y --profile minimal --no-modify-path --default-toolchain "$min_rust_version"
    export PATH="$HOME/.cargo/bin:$PATH"
  fi
  check_rust || fail "Rust $min_rust_version or newer is required to build from source"
  say 'Building nk and nks from source.'
  CARGO_TARGET_DIR="$tmp_dir/target" cargo build --release --bins --manifest-path "$source_dir/comp/Cargo.toml"
  bin_dir=$tmp_dir/target/release
fi

if ! mkdir -p "$install_dir" 2>/dev/null; then
  as_root mkdir -p "$install_dir"
fi
install_as_root=0
if [ ! -w "$install_dir" ]; then install_as_root=1; fi
put() {
  if [ "$install_as_root" -eq 1 ]; then as_root "$@"; else "$@"; fi
}
# Install atomically per binary so interrupted downloads never leave a partial executable.
for name in nk nks; do
  put cp "$bin_dir/$name" "$install_dir/.$name.install-$$"
  put chmod 755 "$install_dir/.$name.install-$$"
  put mv -f "$install_dir/.$name.install-$$" "$install_dir/$name"
done

"$install_dir/nk" --device cpu '"Nmerkar installed" print' > "$tmp_dir/output" || fail 'installed nk could not compile and run a program'
grep -q 'Nmerkar installed' "$tmp_dir/output" || fail 'installed nk returned unexpected output'
"$install_dir/nks" --caps >/dev/null || fail 'installed nks did not start'
say "Installed and verified nk and nks in $install_dir"
case ":$PATH:" in
  *":$install_dir:"*) ;;
  *) say "Add this directory to PATH (or open a new shell after adding it): export PATH=\"$install_dir:\$PATH\"" ;;
esac
