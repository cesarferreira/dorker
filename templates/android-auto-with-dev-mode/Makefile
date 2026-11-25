# Shortcuts for publish.sh. Set REPO once in your shell profile, then `make publish`:
#
#   export DORKER_PUBLISH_REPO=ghcr.io/your-org/gearhead
#
# or pass it each time: make publish REPO=ghcr.io/your-org/gearhead API=37

REPO ?= $(DORKER_PUBLISH_REPO)
API ?= 37

.PHONY: help publish publish-fresh republish run

help:
	@echo "make publish        update the previous version to the latest Android Auto and push it"
	@echo "make publish-fresh  the same, but build from scratch (a clean install)"
	@echo "make republish      redo this machine's push of the latest version (--force)"
	@echo "make run            run the newest published image ($(if $(REPO),$(REPO),REPO):$(API))"
	@echo
	@echo "REPO=$(if $(REPO),$(REPO),(not set: export DORKER_PUBLISH_REPO or pass REPO=...))  API=$(API)"

publish: check-repo
	./publish.sh $(REPO) --api $(API)

publish-fresh: check-repo
	./publish.sh $(REPO) --api $(API) --fresh

republish: check-repo
	./publish.sh $(REPO) --api $(API) --force

run: check-repo
	dorker run -d --name aa $(REPO):$(API)

.PHONY: check-repo
check-repo:
	@test -n "$(REPO)" || { echo "set REPO: make publish REPO=ghcr.io/your-org/gearhead (or export DORKER_PUBLISH_REPO)" >&2; exit 2; }
