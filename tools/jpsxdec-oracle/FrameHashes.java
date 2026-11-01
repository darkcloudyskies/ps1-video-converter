// Test oracle: runs jPSXdec's own decoder (as a library, from its jar) over a
// directory of demuxed frames and prints the same per-frame checksums as
// `psxvc frame-hashes`. See README.md beside this file.

import java.io.File;
import java.lang.reflect.Field;
import java.nio.file.Files;
import java.util.Arrays;
import java.util.regex.Matcher;
import java.util.regex.Pattern;
import jpsxdec.psxvideo.bitstreams.BitStreamUncompressor;
import jpsxdec.psxvideo.mdec.MdecCode;
import jpsxdec.psxvideo.mdec.MdecDecoder_int;
import jpsxdec.psxvideo.mdec.MdecInputStream;
import jpsxdec.psxvideo.mdec.idct.PsxMdecIDCT_int;

public class FrameHashes {

    static final class Fnv {
        long h = 0xcbf29ce484222325L;

        void add(int b) {
            h = (h ^ (b & 0xff)) * 0x00000100000001b3L;
        }

        void addI16(int v) {
            add(v);
            add(v >> 8);
        }

        void addI32(int v) {
            add(v);
            add(v >> 8);
            add(v >> 16);
            add(v >> 24);
        }
    }

    static long coefficientHash(byte[] frame, int blocks) throws Exception {
        MdecInputStream in = BitStreamUncompressor.identifyUncompressor(frame, frame.length);
        MdecCode code = new MdecCode();
        Fnv fnv = new Fnv();
        int[] coefs = new int[64];
        for (int b = 0; b < blocks; b++) {
            Arrays.fill(coefs, 0);
            in.readMdecCode(code);
            int qscale = code.getTop6Bits();
            coefs[0] = code.getBottom10Bits();
            int pos = 0;
            while (!in.readMdecCode(code)) {
                pos += code.getTop6Bits() + 1;
                coefs[MdecInputStream.REVERSE_ZIG_ZAG_LOOKUP_LIST[pos]] = code.getBottom10Bits();
            }
            fnv.add(qscale);
            for (int c : coefs)
                fnv.addI16(c);
        }
        return fnv.h;
    }

    static int[] plane(MdecDecoder_int d, String field) throws Exception {
        Field f = MdecDecoder_int.class.getDeclaredField(field);
        f.setAccessible(true);
        return (int[]) f.get(d);
    }

    static long planeHash(byte[] frame, int width, int height) throws Exception {
        MdecDecoder_int d = new MdecDecoder_int(new PsxMdecIDCT_int(), width, height);
        d.decode(BitStreamUncompressor.identifyUncompressor(frame, frame.length));
        Fnv fnv = new Fnv();
        for (String field : new String[] {"_aiLumaBuffer", "_aiCbBuffer", "_aiCrBuffer"})
            for (int s : plane(d, field))
                fnv.addI32(s);
        return fnv.h;
    }

    /** Prints the IDCT of the block in idct.rs's `busy_block` test. */
    static void printIdctVector() {
        int[] b = new int[64];
        int v = 12345;
        for (int i = 0; i < 64; i++) {
            v = v * 1103515245 + 12345;
            b[i] = (v >> 16) % 300;
        }
        b[0] = 400;
        int[] out = new int[64];
        new PsxMdecIDCT_int().IDCT(b, 0, out);
        System.out.println(Arrays.toString(out));
    }

    public static void main(String[] args) throws Exception {
        if (args.length == 1 && args[0].equals("idct-vector")) {
            printIdctVector();
            return;
        }
        if (args.length != 3) {
            System.err.println("usage: FrameHashes <dir of .bs frames> <width> <height>");
            System.err.println("       FrameHashes idct-vector");
            System.exit(2);
        }
        int width = Integer.parseInt(args[1]);
        int height = Integer.parseInt(args[2]);
        int blocks = (width + 15) / 16 * ((height + 15) / 16) * 6;

        Pattern number = Pattern.compile("\\[#(\\d+)\\]\\.[^.]*$");
        File[] files = new File(args[0]).listFiles((d, n) -> n.endsWith(".bs"));
        Arrays.sort(files);
        for (File f : files) {
            Matcher m = number.matcher(f.getName());
            if (!m.find())
                throw new IllegalStateException("no frame number in " + f.getName());
            int frameNumber = Integer.parseInt(m.group(1));
            byte[] frame = Files.readAllBytes(f.toPath());
            try {
                System.out.printf("%d %016x %016x%n", frameNumber,
                        coefficientHash(frame, blocks), planeHash(frame, width, height));
            } catch (Exception e) {
                System.out.printf("%d error %s%n", frameNumber, e);
            }
        }
    }
}
