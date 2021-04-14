SRC=$(shell find . -name \*.rs | grep -v "^./target")

target/debug/puzzlefs: $(SRC)
	cargo +nightly build

.PHONY: check
check:
	RUST_BACKTRACE=1 cargo +nightly test -- --nocapture

.PHONY: lint
lint: $(SRC)
	rustfmt --check $(SRC)
	cargo +nightly clippy --all-targets --all-features -- -D warnings -A clippy::upper-case-acronyms

.PHONY: fmt
fmt:
	rustfmt --emit files $(SRC)

.PHONY: clean
clean:
	-cargo clean
