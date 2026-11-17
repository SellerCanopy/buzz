import 'dart:async';
import 'dart:convert';

import 'package:hooks_riverpod/hooks_riverpod.dart';
import 'package:http/http.dart' as http;

import '../../shared/relay/relay.dart';

/// The Beehive engine origin, from `--dart-define=BUZZ_BEEHIVE_URL=...`.
///
/// Empty means this build has no Beehive, and the app shows no entry for it.
const _beehiveUrl = String.fromEnvironment('BUZZ_BEEHIVE_URL');

/// The configured Beehive origin without a trailing slash, or empty.
final beehiveUrlProvider = Provider<String>(
  (ref) => _beehiveUrl.replaceFirst(RegExp(r'/+$'), ''),
);

/// A Beehive engine token for the signed-in identity, renewed before it ends.
///
/// The signing key stays with [_authorize]: this class only ever holds the
/// token the engine returns, and logs nothing.
class BeehiveToken {
  BeehiveToken(
    this._authorize,
    this._post, {
    required String baseUrl,
    DateTime Function()? now,
  }) : _url = Uri.parse('$baseUrl/auth/token'),
       _now = now ?? DateTime.now;

  /// Returns the NIP-98 `Authorization` header value for a POST to the URL.
  final Future<String> Function(Uri url) _authorize;

  /// POSTs to the URL with that header and returns the decoded JSON reply.
  final Future<Map<String, dynamic>> Function(Uri url, String authorization)
  _post;
  final Uri _url;
  final DateTime Function() _now;

  /// A token is renewed once less than this remains of its life.
  static const renewBefore = Duration(seconds: 60);

  String? _token;
  DateTime? _expires;
  Future<String>? _pending;

  /// The current token. Concurrent callers share one mint.
  Future<String> token() async {
    final token = _token;
    if (token != null && _expires!.difference(_now()) > renewBefore) {
      return token;
    }
    return _pending ??= _mint().whenComplete(() => _pending = null);
  }

  Future<String> _mint() async {
    final reply = await _post(_url, await _authorize(_url));
    final token = reply['token'];
    final expiresAt = reply['expires_at'];
    if (token is! String || expiresAt is! int) {
      throw const FormatException('Beehive returned an invalid token reply');
    }
    _token = token;
    _expires = DateTime.fromMillisecondsSinceEpoch(expiresAt * 1000);
    return token;
  }
}

/// Mints Beehive tokens with the active identity's NIP-98 signature.
final beehiveTokenProvider = Provider<BeehiveToken>((ref) {
  return BeehiveToken(
    (url) async => buildNip98AuthHeader(
      method: 'POST',
      url: url.toString(),
      bodyBytes: const [],
      nsec: ref.read(relayConfigProvider).nsec,
      pubkey: ref.read(myPubkeyProvider),
    ),
    _postForJson,
    baseUrl: ref.watch(beehiveUrlProvider),
  );
});

Future<Map<String, dynamic>> _postForJson(Uri url, String authorization) async {
  final response = await http
      .post(url, headers: {'Authorization': authorization})
      .timeout(const Duration(seconds: 15));
  if (response.statusCode < 200 || response.statusCode >= 300) {
    throw Exception('Beehive sign-in failed (HTTP ${response.statusCode})');
  }
  final decoded = jsonDecode(response.body);
  if (decoded is! Map<String, dynamic>) {
    throw const FormatException('Beehive returned an invalid token reply');
  }
  return decoded;
}
