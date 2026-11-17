import 'dart:async';
import 'dart:convert';

import 'package:flutter/material.dart';
import 'package:hooks_riverpod/hooks_riverpod.dart';
import 'package:webview_flutter/webview_flutter.dart';

import '../../shared/deeplink/deep_link.dart';
import '../../shared/deeplink/pending_deep_link_provider.dart';
import '../../shared/theme/buzz_icons.dart';
import '../../shared/theme/theme.dart';
import '../../shared/widgets/buzz_loading_indicator.dart';
import '../../shared/widgets/frosted_app_bar.dart';
import '../../shared/widgets/frosted_scaffold.dart';
import '../../shared/widgets/load_error_view.dart';
import 'beehive_token.dart';

/// The channel the page asks for a token on. Messages are request ids.
const _channelName = 'BeehiveNativeChannel';

/// The channel the page asks to open a thread on. Messages are JSON objects
/// with `channelId` and `rootEventId`.
const _threadChannelName = 'BeehiveThreadChannel';

/// Defines `window.__beehiveNative` for the Beehive page.
///
/// `token()` posts a request id to the native channel and resolves when native
/// calls `__beehiveNativeSettle` with the same id. Only the token crosses into
/// the page. `openThread(channelId, rootEventId)` posts the two ids to a second
/// channel, and native opens the app's own thread view for that root event. Safe
/// to run more than once.
@visibleForTesting
const beehiveShim =
    '''
(function () {
  if (window.__beehiveNative) return;
  var waiting = {};
  var next = 0;
  Object.defineProperty(window, '__beehiveNativeSettle', {
    value: function (id, token, failed) {
      var request = waiting[id];
      if (!request) return;
      delete waiting[id];
      if (failed) request.reject(new Error('Beehive sign-in failed'));
      else request.resolve(token);
    },
  });
  Object.defineProperty(window, '__beehiveNative', {
    value: Object.freeze({
      token: function () {
        return new Promise(function (resolve, reject) {
          var id = String(++next);
          waiting[id] = { resolve: resolve, reject: reject };
          $_channelName.postMessage(id);
        });
      },
      openThread: function (channelId, rootEventId) {
        $_threadChannelName.postMessage(
          JSON.stringify({
            channelId: String(channelId),
            rootEventId: String(rootEventId),
          }),
        );
      },
    }),
  });
})();
''';

/// The app's own link to a thread, from a message the page posted to the thread
/// channel, or null when it is not two valid ids.
///
/// The channel must be a channel UUID and the root a 64-character hex event id,
/// the same shapes `buzz://message` links accept, so nothing else the page says
/// can steer navigation. A thread's root event is also the thread's id.
@visibleForTesting
Uri? beehiveThreadLink(String message) {
  final Object? decoded;
  try {
    decoded = jsonDecode(message);
  } on FormatException {
    return null;
  }
  if (decoded is! Map) return null;
  final channelId = decoded['channelId'];
  final rootEventId = decoded['rootEventId'];
  if (channelId is! String || rootEventId is! String) return null;
  final link = Uri(
    scheme: 'buzz',
    host: 'message',
    queryParameters: {
      'channel': channelId,
      'id': rootEventId,
      'thread': rootEventId,
    },
  );
  return parseMessageDeepLink(link) == null ? null : link;
}

/// Beehive in a web view, signed in natively so the key never enters the page.
class BeehiveScreen extends ConsumerStatefulWidget {
  const BeehiveScreen({super.key});

  @override
  ConsumerState<BeehiveScreen> createState() => _BeehiveScreenState();
}

class _BeehiveScreenState extends ConsumerState<BeehiveScreen> {
  late final WebViewController _controller;
  late final Uri _home;
  bool _loading = true;
  bool _failed = false;

  @override
  void initState() {
    super.initState();
    _home = Uri.parse('${ref.read(beehiveUrlProvider)}/beehive/');
    _controller = WebViewController()
      ..setJavaScriptMode(JavaScriptMode.unrestricted)
      ..addJavaScriptChannel(_channelName, onMessageReceived: _onToken)
      ..addJavaScriptChannel(
        _threadChannelName,
        onMessageReceived: _onOpenThread,
      )
      ..setNavigationDelegate(
        NavigationDelegate(
          // The shim hands a token to whatever page asks, so stay on Beehive.
          onNavigationRequest: (request) => _isBeehive(request.url)
              ? NavigationDecision.navigate
              : NavigationDecision.prevent,
          // The cross-platform API has no document-start script, so inject as
          // soon as each stage of the load can take it. The shim is idempotent.
          onPageStarted: (_) => _injectShim(),
          onProgress: (_) => _injectShim(),
          onPageFinished: (_) {
            _injectShim();
            if (mounted && !_failed) setState(() => _loading = false);
          },
          onWebResourceError: (error) {
            if (error.isForMainFrame ?? true) {
              if (mounted) {
                setState(() {
                  _failed = true;
                  _loading = false;
                });
              }
            }
          },
        ),
      );
    unawaited(_controller.loadRequest(_home));
  }

  bool _isBeehive(String url) {
    final uri = Uri.tryParse(url);
    return uri != null &&
        uri.scheme == _home.scheme &&
        uri.host == _home.host &&
        uri.port == _home.port;
  }

  void _injectShim() => unawaited(_controller.runJavaScript(beehiveShim));

  /// Hands a valid thread link to the app's deep link dispatcher, which finds
  /// the channel and pushes the thread view on top of this screen.
  void _onOpenThread(JavaScriptMessage message) {
    final link = beehiveThreadLink(message.message);
    if (link == null || !mounted) return;
    ref.read(pendingDeepLinkProvider.notifier).open(link);
  }

  Future<void> _onToken(JavaScriptMessage message) async {
    final id = jsonEncode(message.message);
    String? token;
    try {
      token = await ref.read(beehiveTokenProvider).token();
    } catch (_) {
      // The page only learns that sign-in failed, never why.
    }
    if (!mounted) return;
    final arguments = token == null
        ? '$id, null, true'
        : '$id, ${jsonEncode(token)}, false';
    unawaited(_controller.runJavaScript('__beehiveNativeSettle($arguments)'));
  }

  void _retry() {
    setState(() {
      _failed = false;
      _loading = true;
    });
    unawaited(_controller.loadRequest(_home));
  }

  @override
  Widget build(BuildContext context) {
    final top = frostedAppBarHeight(context, bottomHeight: Grid.xxs);
    void close() => Navigator.of(context).pop();
    return FrostedScaffold(
      useUtilitySurfaceTheme: true,
      appBar: FrostedAppBar(
        nativeTitle: 'Beehive',
        nativeLeading: IosNavigationAction(
          label: 'Close Beehive',
          symbol: 'xmark',
          onPressed: close,
        ),
        automaticallyImplyLeading: false,
        horizontalInset: Grid.gutter,
        leading: IconButton(
          tooltip: 'Close Beehive',
          icon: const Icon(BuzzIcons.x),
          onPressed: close,
        ),
        title: const Text('Beehive'),
        bottomHeight: Grid.xxs,
        bottom: const SizedBox.expand(),
      ),
      body: Padding(
        padding: EdgeInsets.only(top: top),
        child: _failed
            ? LoadErrorView(message: 'Could not load Beehive', onRetry: _retry)
            : Stack(
                children: [
                  WebViewWidget(controller: _controller),
                  if (_loading)
                    ColoredBox(
                      color: context.colors.surface,
                      child: const Center(child: BuzzLoadingIndicator()),
                    ),
                ],
              ),
      ),
    );
  }
}
