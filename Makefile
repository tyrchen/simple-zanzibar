build:
	@cargo build

check: build test fmt-check clippy

test:
	@cargo nextest run --all-features

bench-baseline:
	@cargo bench --bench baseline -- --sample-size 10

bench-org:
	@cargo bench --bench org_authorization -- --sample-size 10

bench-org-memory:
	@cargo bench --bench org_authorization --no-run
	@target_dir=$${CARGO_TARGET_DIR:-$$(cargo metadata --format-version 1 --no-deps | sed -n 's/.*"target_directory":"\([^"]*\)".*/\1/p')}; \
	bin=$$(find "$$target_dir/release/deps" -maxdepth 1 -type f -name 'org_authorization-*' | while IFS= read -r candidate; do \
		if [ -x "$$candidate" ]; then printf '%s\n' "$$candidate"; fi; \
	done | sort | tail -1); \
	if [ -z "$$bin" ]; then echo "org_authorization benchmark binary not found"; exit 1; fi; \
	for filter in \
		building_blocks/relationship_parse \
		org_authorization/1k_rules/check_direct_group_viewer \
		org_authorization/100k_rules/check_direct_group_viewer \
		org_authorization/1m_rules/check_direct_group_viewer; do \
		echo "== $$filter =="; \
		/usr/bin/time -l "$$bin" "$$filter" --bench --sample-size 10; \
	done

bench-snapshot:
	@cargo bench --bench snapshot -- --sample-size 10

bench-public-api:
	@cargo bench --bench public_api -- --sample-size 10

bench-concurrent-runtime:
	@cargo bench --bench concurrent_runtime -- --sample-size 10

bench-realworld:
	@cargo bench --bench realworld_authorization -- --sample-size 10

bench-perf-optimization:
	@cargo bench --features bench-internals --bench perf_optimization -- --sample-size 10

perf-impact-chart:
	@uv run --script tools/render_perf_impact_chart.py

bench-snapshot-memory:
	@cargo bench --bench snapshot --no-run
	@target_dir=$${CARGO_TARGET_DIR:-$$(cargo metadata --format-version 1 --no-deps | sed -n 's/.*"target_directory":"\([^"]*\)".*/\1/p')}; \
	bin=$$(find "$$target_dir/release/deps" -maxdepth 1 -type f -name 'snapshot-*' | while IFS= read -r candidate; do \
		if [ -x "$$candidate" ]; then printf '%s\n' "$$candidate"; fi; \
	done | sort | tail -1); \
	if [ -z "$$bin" ]; then echo "snapshot benchmark binary not found"; exit 1; fi; \
	snapshot_file=$$(mktemp "$${TMPDIR:-/tmp}/simple-zanzibar-1m.XXXXXX.szsnap"); \
	echo "== prepare snapshot_load_peak_rss/1m fixture =="; \
	SZS_SNAPSHOT_PREPARE_PATH="$$snapshot_file" "$$bin" --bench; \
	for filter in \
		snapshot_load_peak_rss/1m; do \
		echo "== $$filter =="; \
		SZS_SNAPSHOT_LOAD_PATH="$$snapshot_file" SZS_SNAPSHOT_RSS_ONCE=1 /usr/bin/time -l "$$bin" "$$filter" --bench --sample-size 10; \
	done; \
	rm -f "$$snapshot_file"

bench-all: bench-baseline bench-org bench-snapshot bench-public-api bench-concurrent-runtime bench-realworld bench-perf-optimization

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

.PHONY: build check test bench-baseline bench-org bench-org-memory bench-snapshot bench-public-api bench-concurrent-runtime bench-realworld bench-perf-optimization perf-impact-chart bench-snapshot-memory bench-all fmt fmt-check clippy lint release update-submodule
