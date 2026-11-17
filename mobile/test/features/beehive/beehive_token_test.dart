import 'dart:async';

import 'package:buzz/features/beehive/beehive_token.dart';
import 'package:flutter_test/flutter_test.dart';

const _secret = 'Nostr signed-with-secret-key';

void main() {
  late DateTime clock;
  late int signs;
  late int posts;
  late Completer<Map<String, dynamic>>? gate;
  late List<String> printed;
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
}
