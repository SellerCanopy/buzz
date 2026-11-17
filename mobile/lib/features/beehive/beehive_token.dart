import 'dart:async';
import 'dart:convert';

import 'package:flutter/foundation.dart';
import 'package:hooks_riverpod/hooks_riverpod.dart';
import 'package:http/http.dart' as http;

import '../../shared/relay/relay.dart';

/// The Beehive engine origin, from `--dart-define=BUZZ_BEEHIVE_URL=...`.
///
/// Empty means this build has no Beehive, and the app shows no entry for it.
const _beehiveUrl = String.fromEnvironment('BUZZ_BEEHIVE_URL');

/// [url] without trailing slashes, so a path can be appended to it.
@visibleForTesting
String normalizeBeehiveUrl(String url) => url.replaceFirst(RegExp(r'/+$'), '');

/// The configured Beehive origin without a trailing slash, or empty.
final beehiveUrlProvider = Provider<String>(
  (ref) => normalizeBeehiveUrl(_beehiveUrl),
);

/// The client Beehive sign-in goes through. Tests replace it.
final beehiveHttpClientProvider = Provider<http.Client>((ref) {
  final client = http.Client();
  ref.onDispose(client.close);
  return client;
});

/// A Beehive engine token for one identity, renewed before it ends.
///
/// The signing key stays with [_authorize]: this class only ever holds the
/// token the engine returns, and logs nothing. The token belongs to [pubkey]:
/// once [currentPubkey] says the identity changed, a cached or pending token
/// is refused, never handed to the next identity.
class BeehiveToken {
  BeehiveToken(
    this._authorize,
    this._post, {
    required String baseUrl,
    required this.pubkey,
    required this.currentPubkey,
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

  /// The identity this token is for, or null when nobody is signed in.
  final String? pubkey;

  /// The identity signed in now.
  final String? Function() currentPubkey;

  /// A token is renewed once less than this remains of its life.
  static const renewBefore = Duration(seconds: 60);

  String? _token;
  DateTime? _expires;
  Future<String>? _pending;

  /// Forgets the cached token, so the next [token] mints a new one. A mint
  /// already under way is kept: it yields a new token anyway.
  void invalidate() {
    _token = null;
    _expires = null;
  }

  /// The current token. Concurrent callers share one mint. [refresh] drops the
  /// cached token first, for a caller the engine just refused with it.
  Future<String> token({bool refresh = false}) async {
    _checkIdentity();
    if (refresh) invalidate();
    final token = _token;
    if (token != null && _expires!.difference(_now()) > renewBefore) {
      return token;
    }
    return _pending ??= _mint().whenComplete(() => _pending = null);
  }

  void _checkIdentity() {
    if (pubkey == null || currentPubkey() != pubkey) {
      throw StateError('Beehive sign-in is for another identity');
    }
  }

  Future<String> _mint() async {
    final reply = await _post(_url, await _authorize(_url));
    // Signing out or switching community while the engine answered.
    _checkIdentity();
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
///
/// Rebuilt when the identity changes, so a sign-out or a community switch never
/// leaves the old identity's token behind.
final beehiveTokenProvider = Provider<BeehiveToken>((ref) {
  final pubkey = ref.watch(myPubkeyProvider);
  final client = ref.watch(beehiveHttpClientProvider);
  var disposed = false;
  ref.onDispose(() => disposed = true);
  return BeehiveToken(
    (url) async => buildNip98AuthHeader(
      method: 'POST',
      url: url.toString(),
      bodyBytes: const [],
      nsec: ref.read(relayConfigProvider).nsec,
      pubkey: pubkey,
    ),
    beehivePost(client),
    baseUrl: ref.watch(beehiveUrlProvider),
    pubkey: pubkey,
    currentPubkey: () => disposed ? null : ref.read(myPubkeyProvider),
  );
});

/// POSTs to a Beehive URL with the `Authorization` header and returns the JSON
/// object the engine answers with. Anything else, a refusal included, throws.
@visibleForTesting
Future<Map<String, dynamic>> Function(Uri, String) beehivePost(
  http.Client client, {
  Duration timeout = const Duration(seconds: 15),
}) {
  return (url, authorization) async {
    final response = await client
        .post(url, headers: {'Authorization': authorization})
        .timeout(timeout);
    if (response.statusCode < 200 || response.statusCode >= 300) {
      throw Exception('Beehive sign-in failed (HTTP ${response.statusCode})');
    }
    final decoded = jsonDecode(response.body);
    if (decoded is! Map<String, dynamic>) {
      throw const FormatException('Beehive returned an invalid token reply');
    }
    return decoded;
  };
}
