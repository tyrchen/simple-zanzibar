build:
	@cargo build

test:
	@cargo nextest run --all-features

bench-baseline:
	@cargo bench --bench baseline -- --sample-size 10

release:
	@cargo release tag --execute
	@git cliff -o CHANGELOG.md
	@git commit -a -n -m "Update CHANGELOG.md" || true
	@git push origin master
	@cargo release push --execute

update-submodule:
	@git submodule update --init --recursive --remote

.PHONY: build test bench-baseline release update-submodule
