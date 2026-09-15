# mica-core: micad and what ships beside it, packed as Debian packages.
# The scripts do the work; this file only routes. `make` alone is `make help`.

MICA_ARCH ?= arm64

.PHONY: help deps deps-check locks-test release-test rust-gate dbus-policy-test apid-ui-build-contract-test boot-shutdown-test file-transaction-faults deb pool offline package-gate publish lint check

help:
	@echo "  deps                check locks/ and verify every pinned lock against its release (anonymous)"
	@echo "  deps-check          check locks/ offline"
	@echo "  lint                shell hygiene"
	@echo "  locks-test          the lock and pin checker over the specification's vectors, locks.sh and from.sh"
	@echo "  release-test        scripts/build/release.sh against a fixture release"
	@echo "  apid-ui-build-contract-test  the web UI builds in the base image as ignored output"
	@echo "  rust-gate           fmt, clippy -D warnings, nextest, doctests, cargo-deny, OpenAPI, in the mica-build-env rust image"
	@echo "  boot-shutdown-test  the shutdown suite and the lifecycle UAPI unit (BOOT_SHUTDOWN_ARM_ABI=1 adds aarch64)"
	@echo "  file-transaction-faults  mica-deploy transactions interrupted at every IO boundary"
	@echo "  dbus-policy-test    the micad D-Bus policy against a real dbus-daemon (root, host dbus-daemon)"
	@echo "  check               lint, locks-test, release-test, apid-ui-build-contract-test, rust-gate, boot-shutdown-test, file-transaction-faults"
	@echo "  deb                 every producer for MICA_ARCH (amd64|arm64) into _out/debs/<arch>/pool"
	@echo "  pool                deb for both architectures, indexed"
	@echo "  offline             pool from a clean checkout with only the pinned inputs; prints where the outputs are"
	@echo "  package-gate        the package gate over both pools, with a rebuild"
	@echo "  publish             TAG=<YYYYMMDD-HHMM>: attach _out/debs to that release (release workflow only)"

deps:
	bash scripts/build/locks.sh verify
deps-check:
	bash scripts/build/locks.sh check

lint:
	bash scripts/gate/shell-lint.sh
locks-test:
	bash scripts/gate/locks-test.sh
release-test:
	bash scripts/gate/release-test.sh
apid-ui-build-contract-test:
	bash scripts/gate/apid-ui-build-contract-test.sh
	bash crates/mica-apid/ui/run.sh
rust-gate:
	bash scripts/gate/rust-gate.sh
boot-shutdown-test:
	bash scripts/gate/boot-shutdown-test.sh $(if $(BOOT_SHUTDOWN_ARM_ABI),--arm-abi)
file-transaction-faults:
	bash scripts/gate/file-ab-faults/run.sh
dbus-policy-test:
	bash scripts/gate/dbus-policy-test.sh

check: lint locks-test release-test apid-ui-build-contract-test rust-gate boot-shutdown-test file-transaction-faults

deb:
	set -e; rows="$$(bash scripts/deb/producers.sh)"; \
	for p in $$(printf '%s\n' "$$rows" | cut -d' ' -f1); do bash scripts/deb/build.sh --producer "$$p" --arch $(MICA_ARCH); done

pool:
	$(MAKE) deb MICA_ARCH=amd64
	$(MAKE) deb MICA_ARCH=arm64
	bash scripts/deb/repo.sh --arch amd64
	bash scripts/deb/repo.sh --arch arm64

offline:
	bash scripts/build/offline.sh

package-gate:
	bash scripts/deb/package-gate.sh

publish:
	@[ -n "$(TAG)" ] || { echo "usage: make publish TAG=<YYYYMMDD-HHMM>" >&2; exit 1; }
	bash scripts/build/release.sh "$(TAG)"
