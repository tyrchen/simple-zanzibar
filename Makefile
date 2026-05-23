build:
	@cargo build

check: build test fmt-check clippy

test:
	@cargo nextest run --all-features

bench-baseline:
	@cargo bench --bench baseline -- --sample-size 10

bench-org:
	@cargo bench --bench org_authorization -- --sample-size 10

bench-all: bench-baseline bench-org

fmt:
	@cargo +nightly fmt

fmt-check:
	@cargo +nightly fmt --check

clippy:
	@cargo clippy -- -D warnings

lint:
	@cargo clippy -- -D warnings -W clippy::pedantic

release:
	@cargo release tag --execute
	@git cliff -o CHANGELOG.md
	@git commit -a -n -m "Update CHANGELOG.md" || true
	@git push origin master
	@cargo release push --execute

update-submodule:
	@git submodule update --init --recursive --remote

.PHONY: build check test bench-baseline bench-org bench-all fmt fmt-check clippy lint release update-submodule
