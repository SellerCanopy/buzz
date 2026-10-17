# AWS deployment toolkit. Run `make help` to list targets.
SCRIPTS := scripts/aws

.DEFAULT_GOAL := help

.PHONY: help test-aws sync-upstream
help: ## List available targets
	@grep -hE '^[a-zA-Z0-9_.-]+:.*## ' $(MAKEFILE_LIST) | \
		awk 'BEGIN {FS = ":.*## "} {printf "  %-20s %s\n", $$1, $$2}'

test-aws: ## Run the AWS script tests
	@bash tests/aws/run.sh

sync-upstream: ## Merge upstream/main into the current branch (never pushes)
	@bash $(SCRIPTS)/sync-upstream.sh
