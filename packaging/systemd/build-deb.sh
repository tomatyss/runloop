#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/../.." && pwd)"
DEBIAN_DIR="${REPO_ROOT}/debian"

if [ -e "${DEBIAN_DIR}" ]; then
	echo "A top-level debian/ directory already exists; refusing to overwrite it." >&2
	exit 1
fi

cleanup() {
	if [ -d "${DEBIAN_DIR}" ]; then
		rm -rf "${DEBIAN_DIR}"
	fi
}
trap cleanup EXIT

mkdir -p "${DEBIAN_DIR}"
cp -R "${SCRIPT_DIR}/debian/." "${DEBIAN_DIR}/"

pushd "${REPO_ROOT}" >/dev/null
export DEB_BUILD_OPTIONS="${DEB_BUILD_OPTIONS:+${DEB_BUILD_OPTIONS} }parallel=$(nproc)"
dpkg-buildpackage -us -uc -b -rfakeroot
popd >/dev/null

LATEST_DEB="$(ls -1t "${REPO_ROOT}"/../runloop_*_*.deb 2>/dev/null | head -n 1 || true)"
if [ -n "${LATEST_DEB}" ]; then
	echo "Built package: ${LATEST_DEB}"
else
	echo "dpkg-buildpackage completed; inspect the parent directory for artifacts."
fi
