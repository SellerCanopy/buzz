// Drives the real Beehive screen, web view and signer against a local Beehive
// engine on an iOS simulator. Not part of `flutter test`: run it by hand with
//
//   flutter test integration_test/beehive_screen_test.dart -d <simulator id> \
//     --dart-define-from-file=<env.json>
//
// where the env file sets BUZZ_BEEHIVE_URL (the engine origin) and
// BEEHIVE_TEST_NSEC (an identity the engine accepts). The file stays outside
// the repository: the key must never be committed.
//
// BEEHIVE_SHOT_DIR (optional) is a directory the test signals in so a host
// script can take simulator screenshots: it writes `ready-<name>` and waits for
// `done-<name>`.
import 'dart:convert';
import 'dart:io';

import 'package:buzz/features/beehive/beehive_screen.dart';
import 'package:buzz/features/beehive/beehive_token.dart';
import 'package:buzz/shared/relay/relay.dart';
import 'package:buzz/shared/theme/theme.dart';
import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:hooks_riverpod/hooks_riverpod.dart';
import 'package:http/http.dart' as http;
import 'package:integration_test/integration_test.dart';
import 'package:nostr/nostr.dart' as nostr;
import 'package:webview_flutter/webview_flutter.dart';

const _nsec = String.fromEnvironment('BEEHIVE_TEST_NSEC');
const _engine = String.fromEnvironment('BUZZ_BEEHIVE_URL');
const _shotDir = String.fromEnvironment('BEEHIVE_SHOT_DIR');

/// Uses the test identity as the active account, as a signed-in app would.
class _TestAccount extends RelayConfigNotifier {
  @override
  RelayConfig build() => const RelayConfig(baseUrl: _engine, nsec: _nsec);
}

/// Runs [expression] in the page and returns its JSON value.
///
/// The page only ever receives [expression]; nothing secret is put in it.
Future<Object?> _eval(WebViewController controller, String expression) async {
  final result = await controller.runJavaScriptReturningResult(
    'JSON.stringify($expression)',
  );
  // iOS hands back the stringified JSON as is (Android would quote it again).
  return jsonDecode(result.toString());
}

/// Pumps real time until [done] holds or [timeout] passes, then returns it.
Future<bool> _until(
  WidgetTester tester,
  Future<bool> Function() done, {
  Duration timeout = const Duration(seconds: 45),
}) async {
  final end = DateTime.now().add(timeout);
  while (DateTime.now().isBefore(end)) {
    if (await done()) return true;
    await tester.pump(const Duration(milliseconds: 250));
    await Future<void>.delayed(const Duration(milliseconds: 250));
  }
  return done();
}

/// Asks the host to screenshot the simulator, then waits for it to finish.
Future<void> _shot(WidgetTester tester, String name) async {
  if (_shotDir.isEmpty) return;
  final ready = File('$_shotDir/ready-$name')..writeAsStringSync('1');
  final done = File('$_shotDir/done-$name');
  await _until(
    tester,
    () async => done.existsSync(),
    timeout: const Duration(seconds: 60),
  );
  if (ready.existsSync()) ready.deleteSync();
}

void main() {
  IntegrationTestWidgetsFlutterBinding.ensureInitialized();

  testWidgets('the Beehive screen signs in natively and shows the board', (
    tester,
  ) async {
    expect(_nsec, isNotEmpty, reason: 'BEEHIVE_TEST_NSEC is not set');
    expect(_engine, isNotEmpty, reason: 'BUZZ_BEEHIVE_URL is not set');
    final privateHex = nostr.Nip19.decode(payload: _nsec).data;

    final overrides = [relayConfigProvider.overrideWith(_TestAccount.new)];

    // What the engine lists, read with a token minted by the app's own signer.
    final container = ProviderContainer(overrides: overrides);
    addTearDown(container.dispose);
    final token = await container.read(beehiveTokenProvider).token();
    final listed = await http.post(
      Uri.parse('$_engine/kiln.engine.v1.EngineService/ListCards'),
      headers: {
        'Authorization': 'Bearer $token',
        'Content-Type': 'application/json',
      },
      body: '{}',
    );
    expect(listed.statusCode, 200);
    final titles = [
      for (final card in (jsonDecode(listed.body) as Map)['cards'] as List)
        if ((card as Map)['title'] is String) card['title'] as String,
    ];
    expect(titles, isNotEmpty, reason: 'the engine lists no cards');

    final started = DateTime.now();
    await tester.pumpWidget(
      ProviderScope(
        overrides: overrides,
        child: MaterialApp(
          theme: AppTheme.light(),
          home: const BeehiveScreen(),
        ),
      ),
    );
    // The screen keeps its controller private, so wrap the one its widget hands
    // to the platform.
    final controller = WebViewController.fromPlatform(
      tester
          .widget<WebViewWidget>(find.byType(WebViewWidget))
          .platform
          .params
          .controller,
    );

    // The page is on the board: the shim arrived and the page used it.
    final onBoard = await _until(tester, () async {
      try {
        final text =
            await _eval(controller, 'document.body.innerText') as String?;
        return text != null && titles.any(text.contains);
      } catch (_) {
        return false;
      }
    });
    final boardAfter = DateTime.now().difference(started);
    debugPrint(
      'board showed a listed card after ${boardAfter.inMilliseconds} ms',
    );

    final text = await _eval(controller, 'document.body.innerText') as String;
    final shown = titles.where(text.contains).length;
    debugPrint('board shows $shown of ${titles.length} listed cards');
    debugPrint(
      'page text mentions unavailable: ${text.toLowerCase().contains('unavailable')}, '
      'sign-in failed: ${text.toLowerCase().contains('sign-in failed')}',
    );
    expect(onBoard, isTrue, reason: 'no listed card title reached the board');

    // (1) The Beehive theme.
    expect(
      await _eval(controller, 'document.documentElement.dataset.theme'),
      'beehive',
    );

    // (2) The shim.
    expect(
      await _eval(controller, 'typeof window.__beehiveNative.token'),
      'function',
    );
    // A token request settles with a token, not an error. Only a boolean is
    // read back.
    await controller.runJavaScript(
      'window.__tokenCheck = "pending";'
      'window.__beehiveNative.token().then('
      'function (t) { window.__tokenCheck = typeof t === "string" && t.length > 0 ? "ok" : "empty"; },'
      'function () { window.__tokenCheck = "failed"; });',
    );
    expect(
      await _until(
        tester,
        () async => await _eval(controller, 'window.__tokenCheck') != 'pending',
      ),
      isTrue,
    );
    expect(await _eval(controller, 'window.__tokenCheck'), 'ok');

    // (3) The board.
    expect(shown, greaterThan(0));
    await tester.pump(const Duration(milliseconds: 500));
    await _shot(tester, 'cp8-ios-board');

    // (4) No key material in the page.
    expect(
      await _eval(
        controller,
        'Object.getOwnPropertyNames(window.__beehiveNative).sort()',
      ),
      ['openThread', 'token'],
    );
    final storage = jsonEncode(
      await _eval(
        controller,
        '[Object.entries(localStorage), Object.entries(sessionStorage), document.cookie]',
      ),
    );
    final html =
        await _eval(controller, 'document.documentElement.outerHTML') as String;
    for (final (name, haystack) in [
      ('page text', text),
      ('page html', html),
      ('storage', storage),
    ]) {
      expect(haystack.contains(_nsec), isFalse, reason: '$name has the nsec');
      expect(
        haystack.toLowerCase().contains(privateHex.toLowerCase()),
        isFalse,
        reason: '$name has the key hex',
      );
    }

    // Open a card: click its title the way a tap does.
    final title = titles.firstWhere(text.contains);
    final before = await _eval(controller, 'document.body.innerText');
    final clicked = await _eval(
      controller,
      '(function () {'
      'var wanted = ${jsonEncode(title)};'
      'var buttons = document.querySelectorAll(".beehive-tile-title button");'
      'for (var i = 0; i < buttons.length; i++) {'
      'if (buttons[i].textContent.trim() === wanted) { buttons[i].click(); return true; }'
      '}'
      'return false;'
      '})()',
    );
    expect(clicked, isTrue, reason: 'no tile titled "$title"');
    final opened = await _until(
      tester,
      () async => await _eval(controller, 'document.body.innerText') != before,
    );
    expect(opened, isTrue, reason: 'the card page did not open');
    await tester.pump(const Duration(seconds: 2));
    await _shot(tester, 'cp8-ios-card');
  });
}
