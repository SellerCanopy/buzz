variable "aws_region" {
  description = "AWS region for every resource."
  type        = string
  default     = "us-east-1"
}

variable "instance_type" {
  description = "EC2 instance type for the relay server (ARM)."
  type        = string
  default     = "t4g.large"
}

variable "domain" {
  description = "Public host name of the relay, for example buzz.example.com."
  type        = string
}

variable "route53_zone" {
  description = "Name of the Route 53 hosted zone that contains the relay host name."
  type        = string
}

variable "data_volume_gb" {
  description = "Size in GB of the data volume that holds /var/lib/docker (database, git data, media)."
  type        = number
  default     = 50
}
