SRC=$(shell find . -name \*.rs | grep -v "^./target")

.PHONY: debug
debug:
	cargo build

.PHONY: release
release:
	cargo build --release

.PHONY: check
check:
	RUST_BACKTRACE=1 cargo test -- --nocapture

.PHONY: lint
lint: $(SRC)
	rustfmt --check $(SRC)
	cargo clippy --all-targets --all-features -- -D warnings -D rust-2018-idioms -D rust-2021-compatibility -A clippy::upper-case-acronyms

.PHONY: fmt
fmt:
	rustfmt --emit files $(SRC)

.PHONY: clean
clean:
	-cargo clean
