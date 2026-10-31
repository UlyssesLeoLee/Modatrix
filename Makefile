.PHONY: fmt lint test pycheck pytest license-check quality

fmt:
	cargo fmt --all -- --check

lint:
	cargo clippy --workspace --all-targets -- -D warnings

test:
	cargo test --workspace

pycheck:
	python -m compileall agent-service/app agent-service/tests

pytest:
	python -m pytest agent-service/tests

license-check:
	cargo deny check

quality: fmt lint test pycheck pytest
