/* AV1 / libaom 的占位实现。
 *
 * 背景：scrap 会无条件引用 aom 的编码器符号（即使我们只走 VP8 路径），
 * 而我们在 CI 里只静态编了 libvpx/libyuv，没有编 libaom（aom 的源码镜像
 * 不好拿，编一次也很慢）。这些符号只被 scrap 的 AV1 编码路径用到，
 * 我们不会走那条路，所以给一组空实现占位，保证链接能通过。
 *
 * 注意：这是临时的、只用于构建的桩，不是真实实现；调用它们不会有正确行为。
 */
void aom_codec_av1_cx(void) {}
void aom_codec_control(void) {}
void aom_codec_destroy(void) {}
void aom_codec_enc_config_default(void) {}
void aom_codec_enc_config_set(void) {}
void aom_codec_enc_init_ver(void) {}
void aom_codec_encode(void) {}
void aom_codec_get_cx_data(void) {}
void aom_img_wrap(void) {}
