# Service scripts and checks need only the standard library, and their tests only pytest.
SCRIPTS_PYTHON ?= python3
CARGO ?= cargo
# Empty = cargo default (all cores). Set JOBS=2 on a machine with limited memory.
JOBS ?=
JFLAG = $(if $(JOBS),-j $(JOBS))

.PHONY: setup build test-scripts test-workspace test-rust test-service fmt check-public ui test-web
setup:
	python3 eval/setup_eval.py
build:
	$(CARGO) build --locked -p memory-service --bin agent-memoryd --profile fast $(JFLAG)
test-scripts:
	$(SCRIPTS_PYTHON) -m pytest -q scripts/tests
# Every crate in one cargo invocation: separate invocations resolve dependency features differently and
# rebuild shared dependencies. test-rust and test-service run each half on its own.
test-workspace:
	$(CARGO) test --locked --workspace --lib --profile fast $(JFLAG)
test-rust:
	$(CARGO) test --locked -p memory-rules -p memory-config --profile fast $(JFLAG)
test-service:
	$(CARGO) test --locked -p memory-domain -p memory-store -p memory-engine -p memory-algo-mem0 -p memory-service --lib --profile fast $(JFLAG)
fmt:
	$(CARGO) fmt --all -- --check
check-public:
	$(SCRIPTS_PYTHON) scripts/check_public.py
# The web UI (docs/design/web-ui.md §7): only these two targets need Node; cargo never does. `ui` writes web/dist
# for `agent-memoryd serve --ui-dir web/dist`; scripts/install.sh runs it when npm is on PATH.
ui:
	cd web && npm ci && npm run build
test-web:
	cd web && npm ci && npm test
