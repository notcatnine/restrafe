/* Restrafe: the Vorbis setup (codebook) header libvorbis produces for (channels, rate, quality). */
#include <string.h>
#include <vorbis/codec.h>
#include <vorbis/vorbisenc.h>

int restrafe_vorbis_setup_header(int ch, long rate, float q, unsigned char *out, int cap) {
    vorbis_info vi;
    vorbis_comment vc;
    vorbis_dsp_state vd;
    ogg_packet h, hc, hs;
    int n = -1;
    vorbis_info_init(&vi);
    if (vorbis_encode_init_vbr(&vi, ch, rate, q) != 0) {
        vorbis_info_clear(&vi);
        return -1;
    }
    vorbis_comment_init(&vc);
    vorbis_analysis_init(&vd, &vi);
    if (vorbis_analysis_headerout(&vd, &vc, &h, &hc, &hs) == 0 && hs.bytes <= cap) {
        memcpy(out, hs.packet, (size_t)hs.bytes);
        n = (int)hs.bytes;
    }
    vorbis_dsp_clear(&vd);
    vorbis_comment_clear(&vc);
    vorbis_info_clear(&vi);
    return n;
}
