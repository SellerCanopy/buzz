import 'dart:convert';
import 'dart:io';

import 'package:buzz/features/beehive/beehive_screen.dart';
import 'package:flutter_test/flutter_test.dart';

/// Plays the page's side against the real shim, with stub native channels.
const _harness = r'''
const fs = require('fs');
const shim = fs.readFileSync(process.argv[2], 'utf8');
const posts = [];
global.window = global;
global.BeehiveNativeChannel = { postMessage: (m) => posts.push(['token', m]) };
global.BeehiveThreadChannel = { postMessage: (m) => posts.push(['thread', m]) };

(async () => {
  const out = {};
  (0, eval)(shim);
  const native = window.__beehiveNative;
  const settle = window.__beehiveNativeSettle;
  (0, eval)(shim);
  out.sameObjectAfterSecondInjection =
    native === window.__beehiveNative && settle === window.__beehiveNativeSettle;
  out.frozen = Object.isFrozen(native);
  out.keys = Object.keys(native).sort();

  const idOf = (index) => JSON.parse(posts[index][1]).id;

  const resolving = native.token();
  settle(idOf(0), 'tok', false);
  out.resolved = await resolving;

  const failing = native.token();
  settle(idOf(1), null, true);
  try {
    await failing;
    out.rejected = false;
  } catch (error) {
    out.rejected = true;
    out.rejection = error.message;
  }

  const unknown = native.token();
  settle('no-such-id', 'wrong', false);
  settle(idOf(2), 'right', false);
  out.unknownIdIgnored = (await unknown) === 'right';
  // A settled request cannot be settled again.
  settle(idOf(2), 'again', false);

  native.token({ refresh: true });
  native.token({ refresh: false });
  native.token({});
  native.token();
  native.openThread('a', 'b');
  native.openThread(1, 2);
  out.posts = posts;
  console.log(JSON.stringify(out));
})();
''';

/// A Node binary, or null. The repository's pinned one is the fallback for a
/// shell that has not activated Hermit.
String? _node() {
  final candidates = ['node', '${Directory.current.parent.path}/bin/node'];
  for (final candidate in candidates) {
    try {
      if (Process.runSync(candidate, ['--version']).exitCode == 0) {
        return candidate;
      }
    } on ProcessException {
      continue;
    }
  }
  return null;
}

void main() {
  final node = _node();

  test(
    'the shim behaves as the page expects',
    () async {
      final dir = await Directory.systemTemp.createTemp('beehive-shim');
      addTearDown(() => dir.delete(recursive: true));
      final shim = File('${dir.path}/shim.js')..writeAsStringSync(beehiveShim);
      final harness = File('${dir.path}/harness.js')
        ..writeAsStringSync(_harness);

      final result = await Process.run(node!, [harness.path, shim.path]);
      expect(result.exitCode, 0, reason: '${result.stderr}');
      final out = jsonDecode(result.stdout as String) as Map<String, dynamic>;

      expect(out['sameObjectAfterSecondInjection'], isTrue);
      expect(out['frozen'], isTrue);
      expect(out['keys'], ['openThread', 'token']);
      expect(out['resolved'], 'tok');
      expect(out['rejected'], isTrue);
      expect(out['rejection'], 'Beehive sign-in failed');
      expect(out['unknownIdIgnored'], isTrue);

      final posts = (out['posts'] as List).cast<List>();
      final tokens = [
        for (final post in posts)
          if (post[0] == 'token') jsonDecode(post[1] as String),
      ];
      // Three requests earlier in the run, then the four below.
      expect(tokens.map((t) => t['id']), ['1', '2', '3', '4', '5', '6', '7']);
      expect(tokens.skip(3).map((t) => t['refresh']), [
        true,
        false,
        false,
        false,
      ]);
      expect(tokens.take(3).map((t) => t['refresh']), [false, false, false]);

      final threads = [
        for (final post in posts)
          if (post[0] == 'thread') post[1],
      ];
      expect(threads, [
        '{"channelId":"a","rootEventId":"b"}',
        '{"channelId":"1","rootEventId":"2"}',
      ]);
    },
    skip: node == null
        ? 'node is not installed, so the shim JavaScript was not run'
        : false,
  );
}
