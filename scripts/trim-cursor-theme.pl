#!/usr/bin/perl
# Copy an XCursor theme, keeping only a few pixel sizes in each cursor file (themes ship 12+ sizes and are huge).
# usage: trim-cursor-theme.pl SRC_THEME_DIR DEST_THEME_DIR [sizes, default 24,32,48]
use strict; use warnings;
use File::Find; use File::Path qw(make_path); use File::Copy;
my ($src, $dst, $sizes) = @ARGV;
my %keep = map { $_ => 1 } split /,/, ($sizes // "24,32,48");
sub dirname { my $x = shift; $x =~ s{/[^/]*$}{}; $x }
find({ no_chdir => 1, wanted => sub {
    my $p = $File::Find::name; my $rel = substr($p, length($src)); my $to = "$dst$rel";
    if (-l $p) { make_path(dirname($to)); symlink(readlink($p), $to); return }
    if (-d $p) { make_path($to); return }
    make_path(dirname($to));
    if ($rel !~ m{/cursors/}) { copy($p, $to); return }
    open my $f, "<:raw", $p or die "$p: $!"; local $/; my $d = <$f>; close $f;
    my ($magic, $hdr, $ver, $n) = unpack("a4VVV", $d);
    if ($magic ne "Xcur") { copy($p, $to); return }
    my @chunks;
    for my $i (0 .. $n - 1) {
        my ($t, $s, $pos) = unpack("VVV", substr($d, 16 + 12 * $i, 12));
        next unless $keep{$s} && $t == 0xfffd0002;
        my ($w, $h) = unpack("V2", substr($d, $pos + 16, 8));
        push @chunks, [$t, $s, substr($d, $pos, 36 + 4 * $w * $h)];
    }
    if (!@chunks) { copy($p, $to); return }
    my $out = "Xcur" . pack("VVV", 16, $ver, scalar @chunks); my $pos = 16 + 12 * @chunks; my $body = "";
    for my $c (@chunks) { $out .= pack("VVV", $c->[0], $c->[1], $pos + length($body)); $body .= $c->[2] }
    open my $o, ">:raw", $to or die; print $o $out . $body; close $o;
} }, $src);
