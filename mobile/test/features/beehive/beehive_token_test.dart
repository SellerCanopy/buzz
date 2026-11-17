import 'dart:async';
import 'dart:convert';

import 'package:buzz/features/beehive/beehive_token.dart';
import 'package:buzz/shared/relay/relay.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:hooks_riverpod/hooks_riverpod.dart';
import 'package:http/http.dart' as http;
import 'package:http/testing.dart';
import 'package:nostr/nostr.dart' as nostr;

const _secret = 'Nostr signed-with-secret-key';

void main() {
  late DateTime clock;
  late int signs;
  late int posts;
  late Completer<Map<String, dynamic>>? gate;
  late List<String> printed;
  late String? identity;
  Map<String, dynamic> reply = {};
  final urls = <Uri>[];
  final headers = <String>[];

  BeehiveToken build() => BeehiveToken(
    (url) async {
      signs++;
      return _secret;
    },
    (url, authorization) {
      posts++;
      urls.add(url);
      headers.add(authorization);
      return gate?.future ?? Future.value(reply);
    },
    baseUrl: 'https://beehive.example',
    pubkey: 'alice',
    currentPubkey: () => identity,
    now: () => clock,
  );

  Map<String, dynamic> minted(String token, {required int lifeSeconds}) => {
    'token': token,
    'expires_at': clock.millisecondsSinceEpoch ~/ 1000 + lifeSeconds,
  };

  setUp(() {
    clock = DateTime.utc(2026, 1, 1, 12);
    signs = 0;
    posts = 0;
    gate = null;
    printed = [];
    identity = 'alice';
    urls.clear();
    headers.clear();
    reply = minted('first', lifeSeconds: 600);
  });

  test('concurrent callers share one mint', () async {
    gate = Completer();
    final token = build();
    final calls = [token.token(), token.token(), token.token()];
    await Future<void>.delayed(Duration.zero);
    gate!.complete(reply);

    expect(await Future.wait(calls), ['first', 'first', 'first']);
    expect(signs, 1);
    expect(posts, 1);
    expect(urls.single.toString(), 'https://beehive.example/auth/token');
    expect(headers.single, _secret);
  });

  test('reuses the token while more than a minute remains', () async {
    final token = build();
    await token.token();
    clock = clock.add(const Duration(seconds: 538));

    expect(await token.token(), 'first');
    expect(posts, 1);
  });

  test('renews once under a minute remains', () async {
    final token = build();
    await token.token();
    clock = clock.add(const Duration(seconds: 541));
    reply = minted('second', lifeSeconds: 600);

    expect(await token.token(), 'second');
    expect(posts, 2);
  });

  test('a failed mint is not cached and the next call retries', () async {
    final token = build();
    reply = {'error': 'no'};
    await expectLater(token.token(), throwsFormatException);

    reply = minted('recovered', lifeSeconds: 600);
    expect(await token.token(), 'recovered');
    expect(posts, 2);
  });

  test('renews exactly when a minute remains', () async {
    final token = build();
    await token.token();
    clock = clock.add(const Duration(seconds: 540));
    reply = minted('second', lifeSeconds: 600);

    expect(await token.token(), 'second');
    expect(posts, 2);
  });

  test('a token that lives under a minute is minted on every call', () async {
    reply = minted('short', lifeSeconds: 30);
    final token = build();

    expect(await token.token(), 'short');
    expect(await token.token(), 'short');
    expect(await token.token(), 'short');
    expect(posts, 3);
  });

  test('invalidate drops the cached token', () async {
    final token = build();
    await token.token();
    reply = minted('second', lifeSeconds: 600);

    token.invalidate();

    expect(await token.token(), 'second');
    expect(posts, 2);
  });

  test(
    'refresh mints a new token even while the cached one is fresh',
    () async {
      final token = build();
      await token.token();
      reply = minted('second', lifeSeconds: 600);

      expect(await token.token(), 'first');
      expect(await token.token(refresh: true), 'second');
      expect(await token.token(), 'second');
      expect(posts, 2);
    },
  );

  test('a failed refresh leaves nothing cached', () async {
    final token = build();
    await token.token();
    reply = {'error': 'no'};

    await expectLater(token.token(refresh: true), throwsFormatException);

    reply = minted('third', lifeSeconds: 600);
    expect(await token.token(), 'third');
  });

  group('identity', () {
    test('refuses a cached token once the identity changed', () async {
      final token = build();
      await token.token();

      identity = 'bob';

      await expectLater(token.token(), throwsStateError);
      await expectLater(token.token(refresh: true), throwsStateError);
      expect(posts, 1);
    });

    test(
      'refuses a token that was minted while the identity changed',
      () async {
        gate = Completer();
        final token = build();
        final minting = token.token();
        await Future<void>.delayed(Duration.zero);

        identity = null;
        gate!.complete(reply);

        await expectLater(minting, throwsStateError);
        gate = null;
        identity = 'alice';
        reply = minted('second', lifeSeconds: 600);
        expect(await token.token(), 'second');
      },
    );

    test('mints nothing when nobody is signed in', () async {
      identity = null;
      final token = BeehiveToken(
        (url) async => _secret,
        (url, authorization) async => reply,
        baseUrl: 'https://beehive.example',
        pubkey: null,
        currentPubkey: () => identity,
      );

      await expectLater(token.token(), throwsStateError);
    });
  });

  test('never returns or logs the signature', () async {
    final token = build();
    final value = await runZoned(
      () => token.token(),
      zoneSpecification: ZoneSpecification(
        print: (self, parent, zone, line) => printed.add(line),
      ),
    );

    expect(value, 'first');
    expect(value, isNot(contains('secret')));
    expect(printed, isEmpty);

    reply = {'error': 'no'};
    clock = clock.add(const Duration(hours: 1));
    try {
      await token.token();
      fail('expected the invalid reply to throw');
    } on FormatException catch (error) {
      expect(error.toString(), isNot(contains('secret')));
    }
  });
  group('beehivePost', () {
    final url = Uri.parse('https://beehive.example/auth/token');

    Future<Map<String, dynamic>> post(
      http.Client client, {
      Duration timeout = const Duration(seconds: 15),
    }) => beehivePost(client, timeout: timeout)(url, _secret);

    test('returns the JSON object of a 200 and sends the header', () async {
      late http.Request seen;
      final client = MockClient((request) async {
        seen = request;
        return http.Response('{"token":"t","expires_at":1}', 200);
      });

      expect(await post(client), {'token': 't', 'expires_at': 1});
      expect(seen.method, 'POST');
      expect(seen.url, url);
      expect(seen.headers['Authorization'], _secret);
    });

    test('throws on a refusal and on a server error', () async {
      for (final status in [401, 403, 500]) {
        final client = MockClient((_) async => http.Response('no', status));
        await expectLater(
          post(client),
          throwsA(
            isA<Exception>().having(
              (e) => e.toString(),
              'message',
              contains('HTTP $status'),
            ),
          ),
          reason: '$status',
        );
      }
    });

    test('throws on a body that is not a JSON object', () async {
      for (final body in ['[]', '"x"', 'null', 'not json']) {
        final client = MockClient((_) async => http.Response(body, 200));
        await expectLater(post(client), throwsFormatException, reason: body);
      }
    });

    test('gives up when the engine does not answer', () async {
      final never = Completer<http.Response>();
      final client = MockClient((_) => never.future);

      await expectLater(
        post(client, timeout: const Duration(milliseconds: 10)),
        throwsA(isA<TimeoutException>()),
      );
    });
  });

  group('beehiveTokenProvider', () {
    final alice = nostr.Keys(
      'b7e151628aed2a6abf7158809cf4f3c762e7160f38b4da56a784d9045190cfef',
    );
    final bob = nostr.Keys(
      '3c9229289a6125f7fdf1885a77bb12c37a8d3b4962d936f7e3084dece32a3ca1',
    );

    late List<http.Request> requests;
    Completer<void>? hold;
    late ProviderContainer container;

    Map<String, dynamic> signed(http.Request request) {
      final header = request.headers['Authorization']!;
      expect(header, startsWith('Nostr '));
      return jsonDecode(utf8.decode(base64.decode(header.substring(6))))
          as Map<String, dynamic>;
    }

    String tag(Map<String, dynamic> event, String name) =>
        (event['tags'] as List).firstWhere((t) => t[0] == name)[1] as String;

    setUp(() {
      requests = [];
      hold = null;
      final client = MockClient((request) async {
        requests.add(request);
        await hold?.future;
        return http.Response(
          jsonEncode({
            'token': 'for-${signed(request)['pubkey']}',
            'expires_at': DateTime.now().millisecondsSinceEpoch ~/ 1000 + 600,
          }),
          200,
        );
      });
      container = ProviderContainer(
        overrides: [
          beehiveUrlProvider.overrideWithValue('https://beehive.example'),
          beehiveHttpClientProvider.overrideWithValue(client),
          relayConfigProvider.overrideWith(() => _Config(alice.nsec)),
        ],
      );
      addTearDown(container.dispose);
    });

    void signInAs(String? nsec) => container
        .read(relayConfigProvider.notifier)
        .update(baseUrl: 'https://relay.example', nsec: nsec);

    test('signs a POST to the exact token URL for the identity', () async {
      expect(
        await container.read(beehiveTokenProvider).token(),
        'for-${alice.public}',
      );

      final request = requests.single;
      expect(request.method, 'POST');
      expect(request.url.toString(), 'https://beehive.example/auth/token');
      final event = signed(request);
      expect(event['kind'], 27235);
      expect(event['pubkey'], alice.public);
      expect(tag(event, 'u'), 'https://beehive.example/auth/token');
      expect(tag(event, 'method'), 'POST');
      // The request has no body, so the signed payload hash is the empty one.
      expect(
        tag(event, 'payload'),
        'e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855',
      );
    });

    test(
      'mints afresh for the next identity, never reusing the last',
      () async {
        final first = container.read(beehiveTokenProvider);
        expect(await first.token(), 'for-${alice.public}');
        expect(await first.token(), 'for-${alice.public}');
        expect(requests, hasLength(1));

        signInAs(bob.nsec);
        final second = container.read(beehiveTokenProvider);

        expect(second, isNot(same(first)));
        expect(await second.token(), 'for-${bob.public}');
        expect(requests, hasLength(2));
        expect(signed(requests.last)['pubkey'], bob.public);
        // The old instance never serves the new identity, cached or not.
        await expectLater(first.token(), throwsStateError);
      },
    );

    test('drops a token that was minted as the identity changed', () async {
      hold = Completer();
      final first = container.read(beehiveTokenProvider);
      final minting = first.token();
      await Future<void>.delayed(Duration.zero);

      signInAs(bob.nsec);
      hold!.complete();

      await expectLater(minting, throwsStateError);
      hold = null;
      expect(
        await container.read(beehiveTokenProvider).token(),
        'for-${bob.public}',
      );
    });

    test('signing out leaves no token to hand out', () async {
      final first = container.read(beehiveTokenProvider);
      await first.token();

      signInAs(null);

      await expectLater(first.token(), throwsStateError);
      await expectLater(
        container.read(beehiveTokenProvider).token(),
        throwsStateError,
      );
      expect(requests, hasLength(1));
    });
  });

  test('normalizeBeehiveUrl drops trailing slashes only', () {
    expect(normalizeBeehiveUrl(''), '');
    expect(normalizeBeehiveUrl('x'), 'x');
    expect(normalizeBeehiveUrl('x/'), 'x');
    expect(normalizeBeehiveUrl('x//'), 'x');
    expect(normalizeBeehiveUrl('https://a.example/b/'), 'https://a.example/b');
    expect(normalizeBeehiveUrl('https://a.example//b'), 'https://a.example//b');
  });
}

class _Config extends RelayConfigNotifier {
  _Config(this._nsec);

  final String _nsec;

  @override
  RelayConfig build() =>
      RelayConfig(baseUrl: 'https://relay.example', nsec: _nsec);
}
