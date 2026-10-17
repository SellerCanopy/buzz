# AWS deployment toolkit. Run `make help` to list targets.
SCRIPTS := scripts/aws

.DEFAULT_GOAL := help

.PHONY: help test-aws sync-upstream init plan apply destroy
help: ## List available targets
	@grep -hE '^[a-zA-Z0-9_.-]+:.*## ' $(MAKEFILE_LIST) | \
		awk 'BEGIN {FS = ":.*## "} {printf "  %-20s %s\n", $$1, $$2}'

test-aws: ## Run the AWS script tests
	@bash tests/aws/run.sh

sync-upstream: ## Merge upstream/main into the current branch (never pushes)
	@bash $(SCRIPTS)/sync-upstream.sh

init: ## Check the Terraform version and initialize the S3 backend
	@bash $(SCRIPTS)/tf.sh init

plan: ## Check formatting, validate, and show the Terraform plan
	@bash $(SCRIPTS)/tf.sh plan

apply: ## Show the plan, then apply it after you type yes
	@bash $(SCRIPTS)/tf.sh apply

destroy: ## Destroy all infrastructure (you must type BUZZ_DOMAIN to confirm)
	@bash $(SCRIPTS)/tf.sh destroy
