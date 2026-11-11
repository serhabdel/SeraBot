/// Jump-to-latest logic for the chat list. The list is built with
/// `reverse: true`, so scroll offset 0 is the newest message.
library;

/// Offset (in logical pixels) from the newest message that still counts as
/// "at the bottom". Matches the desktop console.
const double kStickBottomPx = 80;

bool nearBottom(double pixels) => pixels < kStickBottomPx;

/// "3", or "99+" for 100 and more; null when there is nothing unseen.
String? unseenBadge(int count) {
  if (count <= 0) {
    return null;
  }
  return count > 99 ? '99+' : '$count';
}

/// Unseen-message count after the list changed. Reading at the bottom resets
/// it; new messages while scrolled away add to it.
int nextUnseen({
  required int unseen,
  required int previousCount,
  required int count,
  required bool atBottom,
}) {
  if (atBottom) {
    return 0;
  }
  if (count > previousCount) {
    return unseen + (count - previousCount);
  }
  return unseen;
}
