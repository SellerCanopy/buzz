import 'package:buzz/features/beehive/beehive_screen.dart';
import 'package:buzz/features/beehive/beehive_token.dart';
import 'package:buzz/shared/deeplink/deep_link.dart';
import 'package:buzz/shared/deeplink/pending_deep_link_provider.dart';
import 'package:buzz/shared/theme/theme.dart';
import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:hooks_riverpod/hooks_riverpod.dart';
import 'package:webview_flutter_platform_interface/webview_flutter_platform_interface.dart';

const _channel = '8b3c4a5e-1f2d-4c6b-9a7e-0d1f2a3b4c5d';
final _root = 'ab' * 32;

String _message(Object channelId, Object rootEventId) =>
    '{"channelId":"$channelId","rootEventId":"$rootEventId"}';

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

  test('the shim defines openThread beside token and posts both ids', () {
    expect(
      beehiveShim,
      contains('openThread: function (channelId, rootEventId)'),
    );
    expect(beehiveShim, contains('BeehiveThreadChannel.postMessage('));
    expect(beehiveShim, contains('BeehiveNativeChannel.postMessage(id)'));
    expect(beehiveShim, contains('if (window.__beehiveNative) return;'));
  });

  group('the screen', () {
    late _FakeWebViewPlatform platform;
    late List<Uri> opened;
    final previous = WebViewPlatform.instance;

    setUp(() {
      platform = _FakeWebViewPlatform();
      WebViewPlatform.instance = platform;
      opened = [];
    });

    tearDown(() {
      if (previous != null) WebViewPlatform.instance = previous;
    });

    Future<void> pump(WidgetTester tester) async {
      await tester.pumpWidget(
        ProviderScope(
          overrides: [
            beehiveUrlProvider.overrideWithValue('https://beehive.example'),
            pendingDeepLinkProvider.overrideWith(
              () => _RecordingLinks(opened.add),
            ),
          ],
          child: MaterialApp(
            theme: AppTheme.light(),
            home: const BeehiveScreen(),
          ),
        ),
      );
    }

    testWidgets('loads the page from the Beehive origin', (tester) async {
      await pump(tester);

      expect(
        platform.controller.loaded.single.uri.toString(),
        contains('https://beehive.example/beehive/'),
      );
      expect(platform.controller.channels.keys, {
        'BeehiveNativeChannel',
        'BeehiveThreadChannel',
      });
    });

    testWidgets('opens the thread the page asks for through the app', (
      tester,
    ) async {
      await pump(tester);

      platform.controller.channels['BeehiveThreadChannel']!(
        JavaScriptMessage(message: _message(_channel, _root)),
      );

      expect(opened, hasLength(1));
      final parsed = parseMessageDeepLink(opened.single)!;
      expect(parsed.channelId, _channel);
      expect(parsed.threadRootId, _root);
    });

    testWidgets('ignores a request with an invalid id', (tester) async {
      await pump(tester);

      for (final message in [
        _message('ab' * 32, _root),
        _message(_channel, 'xyz'),
        'not json',
      ]) {
        platform.controller.channels['BeehiveThreadChannel']!(
          JavaScriptMessage(message: message),
        );
      }

      expect(opened, isEmpty);
    });

    testWidgets('keeps the page on the Beehive origin', (tester) async {
      await pump(tester);
      final navigate = platform.navigation.onNavigationRequest!;

      for (final (url, allowed) in [
        ('https://beehive.example/beehive/cards/1', true),
        ('https://beehive.example.evil.test/beehive/', false),
        ('https://evil.test/beehive/', false),
        ('http://beehive.example/beehive/', false),
        ('https://beehive.example:8443/beehive/', false),
      ]) {
        final decision = await navigate(
          NavigationRequest(url: url, isMainFrame: true),
        );
        expect(
          decision,
          allowed ? NavigationDecision.navigate : NavigationDecision.prevent,
          reason: url,
        );
      }
    });

    testWidgets('injects the shim as the page loads', (tester) async {
      await pump(tester);
      platform.controller.scripts.clear();

      platform.navigation.onPageStarted!('https://beehive.example/beehive/');

      expect(platform.controller.scripts, [beehiveShim]);
    });
  });
}

/// Records the links the page opens instead of listening for real ones.
class _RecordingLinks extends PendingDeepLinkNotifier {
  _RecordingLinks(this._record);

  final void Function(Uri) _record;

  @override
  BuzzDeepLink? build() => null;

  @override
  void open(Uri uri) => _record(uri);
}

class _FakeWebViewPlatform extends WebViewPlatform {
  final controller = _FakeController();
  final navigation = _FakeNavigation();

  @override
  PlatformWebViewController createPlatformWebViewController(
    PlatformWebViewControllerCreationParams params,
  ) => controller;

  @override
  PlatformNavigationDelegate createPlatformNavigationDelegate(
    PlatformNavigationDelegateCreationParams params,
  ) => navigation;

  @override
  PlatformWebViewWidget createPlatformWebViewWidget(
    PlatformWebViewWidgetCreationParams params,
  ) => _FakeWidget(params);
}

class _FakeController extends PlatformWebViewController {
  _FakeController()
    : super.implementation(const PlatformWebViewControllerCreationParams());

  final channels = <String, void Function(JavaScriptMessage)>{};
  final loaded = <LoadRequestParams>[];
  final scripts = <String>[];

  @override
  Future<void> setJavaScriptMode(JavaScriptMode javaScriptMode) async {}

  @override
  Future<void> addJavaScriptChannel(
    JavaScriptChannelParams javaScriptChannelParams,
  ) async {
    channels[javaScriptChannelParams.name] =
        javaScriptChannelParams.onMessageReceived;
  }

  @override
  Future<void> setPlatformNavigationDelegate(
    PlatformNavigationDelegate handler,
  ) async {}

  @override
  Future<void> loadRequest(LoadRequestParams params) async =>
      loaded.add(params);

  @override
  Future<void> runJavaScript(String javaScript) async =>
      scripts.add(javaScript);
}

class _FakeNavigation extends PlatformNavigationDelegate {
  _FakeNavigation()
    : super.implementation(const PlatformNavigationDelegateCreationParams());

  NavigationRequestCallback? onNavigationRequest;
  PageEventCallback? onPageStarted;

  @override
  Future<void> setOnNavigationRequest(
    NavigationRequestCallback onNavigationRequest,
  ) async => this.onNavigationRequest = onNavigationRequest;

  @override
  Future<void> setOnPageStarted(PageEventCallback onPageStarted) async =>
      this.onPageStarted = onPageStarted;

  @override
  Future<void> setOnPageFinished(PageEventCallback onPageFinished) async {}

  @override
  Future<void> setOnProgress(ProgressCallback onProgress) async {}

  @override
  Future<void> setOnWebResourceError(
    WebResourceErrorCallback onWebResourceError,
  ) async {}
}

class _FakeWidget extends PlatformWebViewWidget {
  _FakeWidget(super.params) : super.implementation();

  @override
  Widget build(BuildContext context) => const SizedBox.expand();
}
