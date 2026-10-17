# AWS deployment toolkit. Run `make help` to list targets.
SCRIPTS := scripts/aws

.DEFAULT_GOAL := help

.PHONY: help test-aws
help: ## List available targets
	@grep -hE '^[a-zA-Z0-9_.-]+:.*## ' $(MAKEFILE_LIST) | \
		awk 'BEGIN {FS = ":.*## "} {printf "  %-20s %s\n", $$1, $$2}'

test-aws: ## Run the AWS script tests
	@bash tests/aws/run.sh
