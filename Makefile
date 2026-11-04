# Notary. The sandbox engine is explicit (no auto-detection): make <target> ENGINE=podman|docker
ENGINE ?=
export NOTARY_SANDBOX_ENGINE := $(ENGINE)
NOTARY_LOG := $(HOME)/.local/share/switch-notary/notary.log

.PHONY: help up down reset build test lint sandbox-image install-offline install-live doctor run e2e demo-offline verify-sample need-engine

help:
	@echo "make verify-sample                 check the committed sample receipt (needs only Rust)"
	@echo "make demo-offline ENGINE=podman    Switch + Mattermost + Notary + 6 scripted scenarios, no model keys"
	@echo "make up | install-live | run       the live desk with Claude Code and OpenCode (see README)"
	@echo "make test | lint | doctor | down | reset"

need-engine:
	@test "$(ENGINE)" = podman -o "$(ENGINE)" = docker || { echo "set ENGINE=podman or ENGINE=docker"; exit 1; }

up:
	scripts/up.sh

down:
	cd infra && docker compose --env-file .env -f compose.yml --profile collab down

reset: ## stops the stack and deletes its volumes, the Notary's data and keys
	-cd infra && docker compose --env-file .env -f compose.yml --profile collab down -v
	rm -rf infra/.env infra/notary.env "$(HOME)/.local/share/switch-notary" "$(HOME)/.config/switch-notary"

build:
	cargo build --release

test:
	cargo test

lint:
	cargo fmt --all --check
	cargo clippy --all-targets -- -D warnings

sandbox-image: need-engine
	scripts/sandbox-image.sh

install-offline: need-engine
	scripts/install.sh offline

install-live: need-engine
	scripts/install.sh live

doctor: build
	scripts/notary.sh doctor

run: build
	scripts/notary.sh run

e2e:
	python3 -I e2e/scenarios.py

demo-offline: need-engine up sandbox-image install-offline build
	@mkdir -p "$(dir $(NOTARY_LOG))"
	scripts/notary.sh doctor
	@scripts/notary.sh run > "$(NOTARY_LOG)" 2>&1 & echo $$! > .notary.pid; \
	  for i in $$(seq 1 30); do grep -q "connected to Switch" "$(NOTARY_LOG)" && break; sleep 1; done; \
	  echo "watch it live: http://localhost:8090 (no login)"; \
	  python3 -I e2e/scenarios.py; status=$$?; kill $$(cat .notary.pid); rm -f .notary.pid; exit $$status

verify-sample:
	cargo run --quiet --release -p notary-core --bin notary-verify -- samples/receipt-wo-1.json
