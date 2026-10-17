data "aws_route53_zone" "main" {
  name         = var.route53_zone
  private_zone = false
}

resource "aws_route53_record" "relay" {
  zone_id = data.aws_route53_zone.main.zone_id
  name    = var.domain
  type    = "A"
  ttl     = 300
  records = [aws_eip.relay.public_ip]
}
