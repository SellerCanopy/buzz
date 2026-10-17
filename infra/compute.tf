locals {
  # Docker Compose CLI plugin installed by the first-boot script. The checksum
  # is the one published with the release for docker-compose-linux-aarch64.
  # The TLS override in deploy/compose needs 2.24.4 or newer.
  compose_version = "5.6.0"
  compose_sha256  = "733ec76717ceb59052a9609b9dadfb523b2df8eab57a54212872d10a58078ea2"
}

data "aws_ssm_parameter" "al2023_arm64" {
  name = "/aws/service/ami-amazon-linux-latest/al2023-ami-kernel-default-arm64"
}

resource "aws_instance" "server" {
  ami                    = nonsensitive(data.aws_ssm_parameter.al2023_arm64.value)
  instance_type          = var.instance_type
  subnet_id              = aws_subnet.public.id
  vpc_security_group_ids = [aws_security_group.relay.id]
  iam_instance_profile   = aws_iam_instance_profile.server.name

  user_data = templatefile("${path.module}/user-data.sh.tftpl", {
    data_volume_id  = aws_ebs_volume.data.id
    compose_version = local.compose_version
    compose_sha256  = local.compose_sha256
  })
  user_data_replace_on_change = false

  metadata_options {
    http_endpoint = "enabled"
    http_tokens   = "required"
  }

  root_block_device {
    volume_type = "gp3"
    volume_size = 30
    encrypted   = true
  }

  tags = {
    Name = "buzz"
  }

  # A new AMI release or an edited first-boot script must never replace the server.
  lifecycle {
    ignore_changes = [ami, user_data]
  }
}

resource "aws_ebs_volume" "data" {
  availability_zone = aws_subnet.public.availability_zone
  size              = var.data_volume_gb
  type              = "gp3"
  encrypted         = true

  tags = {
    Name     = "buzz-data"
    Snapshot = "daily"
  }
}

# Shows up on the instance as an NVMe device. The first-boot script finds it by
# its by-id link, not by this device name.
resource "aws_volume_attachment" "data" {
  device_name = "/dev/sdf"
  volume_id   = aws_ebs_volume.data.id
  instance_id = aws_instance.server.id
}

resource "aws_eip" "relay" {
  domain = "vpc"

  tags = {
    Name = "buzz-relay"
  }

  depends_on = [aws_internet_gateway.main]
}

resource "aws_eip_association" "relay" {
  instance_id   = aws_instance.server.id
  allocation_id = aws_eip.relay.id
}

data "aws_iam_policy_document" "dlm_trust" {
  statement {
    actions = ["sts:AssumeRole"]

    principals {
      type        = "Service"
      identifiers = ["dlm.amazonaws.com"]
    }
  }
}

resource "aws_iam_role" "dlm" {
  name               = "buzz-dlm"
  assume_role_policy = data.aws_iam_policy_document.dlm_trust.json
}

resource "aws_iam_role_policy_attachment" "dlm" {
  role       = aws_iam_role.dlm.name
  policy_arn = "arn:aws:iam::aws:policy/service-role/AWSDataLifecycleManagerServiceRole"
}

# Snapshots only volumes tagged Snapshot = daily, which is the data volume.
resource "aws_dlm_lifecycle_policy" "daily" {
  description        = "Daily snapshot of the Buzz data volume and keep 7"
  execution_role_arn = aws_iam_role.dlm.arn
  state              = "ENABLED"

  policy_details {
    resource_types = ["VOLUME"]

    target_tags = {
      Snapshot = "daily"
    }

    schedule {
      name = "daily"

      create_rule {
        interval      = 24
        interval_unit = "HOURS"
        times         = ["03:00"]
      }

      retain_rule {
        count = 7
      }

      copy_tags = true
    }
  }

  depends_on = [aws_iam_role_policy_attachment.dlm]
}
