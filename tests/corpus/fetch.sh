#!/usr/bin/env bash
#
# Downloads the public mbox corpus into target/mbox-corpus, checking each
# file against its pinned SHA-256. The files are public mailing-list
# archives, so they are fetched rather than committed.
#
#   tests/corpus/fetch.sh [dir]     dir defaults to target/mbox-corpus
#
# Then run the suite with: cargo test --test corpus -- --ignored

set -euo pipefail

dir="${1:-$(dirname "$0")/../../target/mbox-corpus}"
mkdir -p "$dir"

fetch() {
  local name="$1" sha="$2"
  shift 2
  local file="$dir/$name"

  if [ -f "$file" ] && echo "$sha  $file" | sha256sum -c --status; then
    echo "ok $name (cached)"
    return
  fi

  curl -fsSL --retry 3 "$@" -o "$file.part"
  if gzip -t "$file.part" 2>/dev/null; then
    gzip -dc "$file.part" > "$file"
    rm "$file.part"
  else
    mv "$file.part" "$file"
  fi

  if ! echo "$sha  $file" | sha256sum -c --status; then
    echo "FAILED $name: checksum mismatch, the archive changed upstream" >&2
    echo "  got $(sha256sum "$file" | cut -d' ' -f1)" >&2
    exit 1
  fi
  echo "ok $name"
}

# GNU Mailman: mboxo, MAILER-DAEMON senders, quoted >From body lines.
fetch gnu_help_bash_2024_01.mbox \
  5db0ac527a8feaf3ae843ddd440c0ed0c5a86a14f60296d2bbd1f5051b07880c \
  "https://lists.gnu.org/archive/mbox/help-bash/2024-01"

# Apache Pony Mail: two spaces before the From_ date, large MIME parts.
fetch apache_httpd_dev_2024_01.mbox \
  142627878514c2a0602c0a23c86c19d35e9115c990ba223637332d7acd191819 \
  "https://lists.apache.org/api/mbox.lua?list=dev&domain=httpd.apache.org&d=2024-01"

# public-inbox (lore.kernel.org): mboxrd, epoch From_ lines, inline patches.
fetch lore_git_2024_01_02.mbox \
  5dfaf18c29127c147982fff7c474c93ba2723062b650b7e318794250363b9df6 \
  -X POST -d x=full "https://lore.kernel.org/git/?q=d:2024-01-02..2024-01-03&x=m"

# GNU Mailman, 2001: pre-MIME-era mail, eight-bit headers.
fetch gnu_bug_bash_2001_06.mbox \
  7028a6890d8cd4e1f69282a19f2df09e55a56d58f679615689facd26bd7f7564 \
  "https://lists.gnu.org/archive/mbox/bug-bash/2001-06"

# GNU Mailman, 2005: 1500 messages, quoted >From lines.
fetch gnu_emacs_devel_2005_03.mbox \
  71c92b219950927764aabb45a885f470ee30beba2a7ce94f6ecc7fc62a594b1e \
  "https://lists.gnu.org/archive/mbox/emacs-devel/2005-03"

# public-inbox, one month: 22 MB, 2900 messages, the size case.
fetch lore_git_2024_01.mbox \
  b65618bffadadbe8e0e36715b777e057c62314aabc1ac21b7a921f621d492cbd \
  -X POST -d x=full "https://lore.kernel.org/git/?q=d:2024-01-01..2024-02-01&x=m"
