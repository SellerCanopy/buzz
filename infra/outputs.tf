output "ecr_repository_url" {
  description = "URL of the relay image repository."
  value       = aws_ecr_repository.relay.repository_url
}

output "agents_ecr_repository_url" {
  description = "URL of the agents image repository."
  value       = aws_ecr_repository.agents.repository_url
}

output "backup_bucket" {
  description = "Name of the private S3 bucket that holds database backups."
  value       = aws_s3_bucket.backups.bucket
}
