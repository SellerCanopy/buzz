# AWS deployment toolkit. Run `make help` to list targets.
SCRIPTS := scripts/aws
# TAG is empty unless given. build and push fall back to the short git SHA;
# deploy and rollback pass TAG through so the scripts can resolve it themselves.
BUILD_TAG = $(or $(TAG),$(shell git rev-parse --short HEAD))

.DEFAULT_GOAL := help

.PHONY: help test-aws sync-upstream init plan apply destroy secrets build push deploy rollback
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

secrets: ## Create the relay secrets in SSM Parameter Store if absent
	@bash $(SCRIPTS)/secrets.sh

build: ## Build the relay image for linux/arm64 (TAG=<tag>, default short git SHA)
	@bash $(SCRIPTS)/build.sh $(BUILD_TAG)

push: ## Build the relay image and push it to ECR (TAG=<tag>, default short git SHA)
	@bash $(SCRIPTS)/build.sh $(BUILD_TAG) --push

deploy: ## Back up, build, push, and deploy the current commit (DRY_RUN=1 to preview)
	@bash $(SCRIPTS)/deploy.sh $(TAG)

rollback: ## Redeploy an earlier image (TAG=<tag>; without TAG it uses the last good image, which is the running tag after a healthy deploy, so a bad but healthy release needs an explicit TAG)
	@ROLLBACK=1 bash $(SCRIPTS)/deploy.sh $(TAG)
