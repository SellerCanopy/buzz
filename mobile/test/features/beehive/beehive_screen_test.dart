import 'dart:async';

import 'package:buzz/features/beehive/beehive_screen.dart';
import 'package:buzz/features/beehive/beehive_token.dart';
import 'package:buzz/shared/deeplink/deep_link.dart';
import 'package:buzz/shared/deeplink/pending_deep_link_provider.dart';
import 'package:buzz/shared/theme/theme.dart';
import 'package:buzz/shared/widgets/load_error_view.dart';
import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:hooks_riverpod/hooks_riverpod.dart';
import 'package:webview_flutter_platform_interface/webview_flutter_platform_interface.dart';

import 'fake_webview_platform.dart';

const _channel = '8b3c4a5e-1f2d-4c6b-9a7e-0d1f2a3b4c5d';
final _root = 'ab' * 32;
final _home = Uri.parse('https://beehive.example/beehive/');

String _message(Object channelId, Object rootEventId) =>
    '{"channelId":"$channelId","rootEventId":"$rootEventId"}';

String _tokenRequest(String id, {bool refresh = false}) =>
    '{"id":"$id","refresh":$refresh}';

void main() {
  group('beehiveThreadLink', () {
    test('turns a channel UUID and a root event id into a thread link', () {
      final link = beehiveThreadLink(_message(_channel, _root))!;

      final parsed = parseMessageDeepLink(link)!;
      expect(parsed.channelId, _channel);
      expect(parsed.messageId, _root);
      expect(parsed.threadRootId, _root);
    });

    test('accepts upper case ids and lowers them', () {
      final link = beehiveThreadLink(
        _message(_channel.toUpperCase(), _root.toUpperCase()),
      )!;

      final parsed = parseMessageDeepLink(link)!;
      expect(parsed.channelId, _channel);
      expect(parsed.threadRootId, _root);
    });

    test('refuses an id that is not the right shape', () {
      for (final (channelId, rootEventId) in [
        ('', _root),
        (_channel, ''),
        ('ab' * 32, _root), // A hex id is not a channel.
        (_channel, 'ab' * 31),
        (_channel, '${'ab' * 31}zz'),
        (_channel, 'ab' * 33),
        (_channel, _channel),
        ('$_channel&thread=${'cd' * 32}', _root),
        (_channel, '$_root&channel=$_channel'),
        ('buzz://channel/$_channel', _root),
      ]) {
        expect(
          beehiveThreadLink(_message(channelId, rootEventId)),
          isNull,
          reason: '$channelId, $rootEventId',
        );
      }
    });

    test('refuses a message that is not an object of two strings', () {
      for (final message in [
        '',
        'not json',
        '[]',
        '"x"',
        'null',
        '{}',
        '{"channelId":"$_channel"}',
        '{"rootEventId":"$_root"}',
        '{"channelId":1,"rootEventId":"$_root"}',
        '{"channelId":"$_channel","rootEventId":null}',
      ]) {
        expect(beehiveThreadLink(message), isNull, reason: message);
      }
    });
  });

  group('beehiveTokenRequest', () {
    test('reads the id and the refresh flag', () {
      expect(beehiveTokenRequest(_tokenRequest('7')), (
        id: '7',
        refresh: false,
      ));
      expect(beehiveTokenRequest(_tokenRequest('7', refresh: true)), (
        id: '7',
        refresh: true,
      ));
    });

    test('only a true refresh counts, and an id is required', () {
      expect(beehiveTokenRequest('{"id":"7","refresh":"yes"}')!.refresh, false);
      expect(beehiveTokenRequest('{"id":"7"}')!.refresh, false);
      for (final message in ['', '7', '[]', '{}', '{"id":7}', '{"id":""}']) {
        expect(beehiveTokenRequest(message), isNull, reason: message);
      }
    });
  });

  group('isBeehivePage', () {
    test('accepts pages under the Beehive path of the Beehive origin', () {
      for (final url in [
        'https://beehive.example/beehive/',
        'https://beehive.example/beehive/x',
        'https://beehive.example/beehive/a/b?c=d#e',
        // Dart resolves a dot segment, and the result stays under the path.
        'https://beehive.example/beehive/./x',
      ]) {
        expect(isBeehivePage(_home, url), isTrue, reason: url);
      }
    });

    test('refuses everything else', () {
      for (final url in [
        'javascript:alert(1)',
        'data:text/html,<script>1</script>',
        'about:blank',
        'file:///etc/passwd',
        'https://beehive.example@evil.test/',
        'https://beehive.example@evil.test/beehive/',
        'https://evil.test@beehive.example/',
        'https://evil.test@beehive.example/beehive/',
        'not a url',
        '',
        'https://beehive.example/',
        'https://beehive.example/beehive',
        'https://beehive.example/relay-page',
        'https://beehive.example/beehive/../admin',
        'https://beehive.example/beehive/%2e%2e/admin',
        'http://beehive.example/beehive/',
        'https://beehive.example:8443/beehive/',
        'https://beehive.example.evil.test/beehive/',
        'https://evil.test/beehive/',
      ]) {
        expect(isBeehivePage(_home, url), isFalse, reason: url);
      }
    });

    test('follows a Beehive URL that has its own path', () {
      final home = Uri.parse('https://example.org/team/beehive/');
      expect(isBeehivePage(home, 'https://example.org/team/beehive/x'), isTrue);
      expect(isBeehivePage(home, 'https://example.org/beehive/x'), isFalse);
    });
  });

  group('the screen', () {
    late FakeWebViewPlatform platform;
    late FakeController web;
    late int mints;
    late FutureOr<Map<String, dynamic>> Function() mint;
    final previous = WebViewPlatform.instance;

    setUp(() {
      platform = FakeWebViewPlatform();
      web = platform.controller;
      WebViewPlatform.instance = platform;
      PendingDeepLinkNotifier.debugUriStreamOverride = const Stream.empty();
      mints = 0;
      mint = () => {
        'token': 'tok',
        'expires_at': DateTime.now().millisecondsSinceEpoch ~/ 1000 + 600,
      };
    });

    tearDown(() {
      if (previous != null) WebViewPlatform.instance = previous;
      PendingDeepLinkNotifier.debugUriStreamOverride = null;
    });

    Future<void> pump(WidgetTester tester) async {
      await tester.pumpWidget(
        ProviderScope(
          overrides: [
            beehiveUrlProvider.overrideWithValue('https://beehive.example'),
            beehiveTokenProvider.overrideWithValue(
              BeehiveToken(
                (_) async => 'authorization',
                (_, _) async {
                  mints++;
                  return mint();
                },
                baseUrl: 'https://beehive.example',
                pubkey: 'me',
                currentPubkey: () => 'me',
              ),
            ),
          ],
          child: MaterialApp(
            theme: AppTheme.light(),
            home: const BeehiveScreen(),
          ),
        ),
      );
    }

    BuzzDeepLink? pendingLink(WidgetTester tester) => ProviderScope.containerOf(
      tester.element(find.byType(BeehiveScreen)),
    ).read(pendingDeepLinkProvider);

    List<String> settles() => web.scripts
        .where((s) => s.startsWith('__beehiveNativeSettle'))
        .toList();

    void post(String channel, String message) =>
        web.channels[channel]!(JavaScriptMessage(message: message));

    testWidgets('loads the Beehive page with scripts on and both channels', (
      tester,
    ) async {
      await pump(tester);

      expect(web.loaded.single.uri, _home);
      expect(web.javaScriptMode, JavaScriptMode.unrestricted);
      expect(web.channels.keys, {
        'BeehiveNativeChannel',
        'BeehiveThreadChannel',
      });
    });

    group('openThread', () {
      testWidgets('parks the thread link for the app to open', (tester) async {
        await pump(tester);

        post('BeehiveThreadChannel', _message(_channel, _root));
        await tester.pump();

        final link = pendingLink(tester) as MessageDeepLink;
        expect(link.channelId, _channel);
        expect(link.messageId, _root);
        expect(link.threadRootId, _root);
      });

      testWidgets('ignores a request with an invalid id', (tester) async {
        await pump(tester);

        for (final message in [
          _message('ab' * 32, _root),
          _message(_channel, 'xyz'),
          'not json',
        ]) {
          post('BeehiveThreadChannel', message);
        }
        await tester.pump();

        expect(pendingLink(tester), isNull);
      });

      testWidgets('ignores a request while the web view is off Beehive', (
        tester,
      ) async {
        await pump(tester);

        for (final url in [
          'https://beehive.example/',
          'https://evil.test/beehive/',
          'about:blank',
          null,
        ]) {
          web.url = url;
          post('BeehiveThreadChannel', _message(_channel, _root));
          await tester.pump();
          expect(pendingLink(tester), isNull, reason: '$url');
        }
      });
    });

    group('the token channel', () {
      testWidgets('answers with the token, then with a failure', (
        tester,
      ) async {
        await pump(tester);

        post('BeehiveNativeChannel', _tokenRequest('7'));
        await tester.pump();
        expect(web.scripts.last, '__beehiveNativeSettle("7", "tok", false)');
        expect(mints, 1);

        mint = () => throw Exception('engine down');
        post('BeehiveNativeChannel', _tokenRequest('7', refresh: true));
        await tester.pump();
        expect(web.scripts.last, '__beehiveNativeSettle("7", null, true)');
        expect(mints, 2);
      });

      testWidgets('a request without refresh reuses the token', (tester) async {
        await pump(tester);

        post('BeehiveNativeChannel', _tokenRequest('1'));
        await tester.pump();
        post('BeehiveNativeChannel', _tokenRequest('2'));
        await tester.pump();

        expect(mints, 1);
        expect(settles(), [
          '__beehiveNativeSettle("1", "tok", false)',
          '__beehiveNativeSettle("2", "tok", false)',
        ]);
      });

      testWidgets('a request that is not a request is ignored', (tester) async {
        await pump(tester);

        post('BeehiveNativeChannel', '7');
        await tester.pump();

        expect(mints, 0);
        expect(settles(), isEmpty);
      });

      testWidgets('never mints for a page that is off Beehive', (tester) async {
        await pump(tester);
        web.url = 'https://beehive.example/';

        post('BeehiveNativeChannel', _tokenRequest('7'));
        await tester.pump();

        expect(mints, 0);
        expect(settles(), isEmpty);
      });

      testWidgets('does not reply once the page left Beehive', (tester) async {
        await pump(tester);
        final gate = Completer<Map<String, dynamic>>();
        mint = () => gate.future;

        post('BeehiveNativeChannel', _tokenRequest('7'));
        await tester.pump();
        web.url = 'https://beehive.example/relay-page';
        gate.complete({
          'token': 'tok',
          'expires_at': DateTime.now().millisecondsSinceEpoch ~/ 1000 + 600,
        });
        await tester.pump();

        expect(settles(), isEmpty);
      });

      testWidgets('runs nothing after the screen is gone', (tester) async {
        await pump(tester);
        final gate = Completer<Map<String, dynamic>>();
        mint = () => gate.future;

        post('BeehiveNativeChannel', _tokenRequest('7'));
        await tester.pump();
        await tester.pumpWidget(const SizedBox());
        final before = web.scripts.length;
        gate.complete({
          'token': 'tok',
          'expires_at': DateTime.now().millisecondsSinceEpoch ~/ 1000 + 600,
        });
        await tester.pump();

        expect(web.scripts.length, before);
        expect(settles(), isEmpty);
      });
    });

    testWidgets('keeps every frame on the Beehive pages', (tester) async {
      await pump(tester);
      final navigate = platform.navigation.onNavigationRequest!;

      for (final (url, allowed) in [
        ('https://beehive.example/beehive/', true),
        ('https://beehive.example/beehive/x', true),
        ('https://beehive.example/', false),
        ('https://beehive.example/relay-page', false),
        ('https://beehive.example.evil.test/beehive/', false),
        ('https://evil.test@beehive.example/', false),
        ('https://beehive.example@evil.test/', false),
        ('javascript:alert(1)', false),
        ('data:text/html,hi', false),
        ('about:blank', false),
        ('file:///etc/passwd', false),
        ('not a url', false),
      ]) {
        for (final isMainFrame in [true, false]) {
          final decision = await navigate(
            NavigationRequest(url: url, isMainFrame: isMainFrame),
          );
          expect(
            decision,
            allowed ? NavigationDecision.navigate : NavigationDecision.prevent,
            reason: '$url, main frame $isMainFrame',
          );
        }
      }
    });

    testWidgets('injects the shim at every stage of the load', (tester) async {
      await pump(tester);
      final navigation = platform.navigation;

      for (final stage in <void Function()>[
        () => navigation.onPageStarted!('https://beehive.example/beehive/'),
        () => navigation.onProgress!(50),
        () => navigation.onPageFinished!('https://beehive.example/beehive/'),
      ]) {
        web.scripts.clear();
        stage();
        expect(web.scripts, [beehiveShim]);
      }
    });

    group('a failed load', () {
      testWidgets(
        'shows the error for the main frame, and Retry reloads once',
        (tester) async {
          await pump(tester);

          platform.navigation.onWebResourceError!(
            const WebResourceError(
              errorCode: -1009,
              description: 'offline',
              isForMainFrame: true,
            ),
          );
          await tester.pump();
          expect(find.byType(LoadErrorView), findsOneWidget);
          expect(web.loaded, hasLength(1));

          await tester.tap(find.byKey(const ValueKey('load-error-retry')));
          await tester.pump();

          expect(web.loaded.map((r) => r.uri), [_home, _home]);
          expect(find.byType(LoadErrorView), findsNothing);
        },
      );

      testWidgets('does not show the error for a sub-frame', (tester) async {
        await pump(tester);

        platform.navigation.onWebResourceError!(
          const WebResourceError(
            errorCode: -1009,
            description: 'offline',
            isForMainFrame: false,
          ),
        );
        await tester.pump();

        expect(find.byType(LoadErrorView), findsNothing);
        expect(web.loaded, hasLength(1));
      });
    });
  });
}
