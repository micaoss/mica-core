/* Compile-only Linux UAPI proof. No device is opened by this fixture. */
#include <stddef.h>
#include <linux/loop.h>
#include <linux/watchdog.h>
#include <sys/ioctl.h>
_Static_assert(sizeof(struct watchdog_info) == 40, "watchdog size");
_Static_assert(_Alignof(struct watchdog_info) == 4, "watchdog alignment");
_Static_assert(offsetof(struct watchdog_info, options) == 0, "watchdog options");
_Static_assert(offsetof(struct watchdog_info, firmware_version) == 4, "watchdog firmware");
_Static_assert(offsetof(struct watchdog_info, identity) == 8, "watchdog identity");
_Static_assert(sizeof(struct loop_info64) == 232, "loop size");
_Static_assert(_Alignof(struct loop_info64) == 8, "loop alignment");
_Static_assert(offsetof(struct loop_info64, lo_device) == 0, "loop device");
_Static_assert(offsetof(struct loop_info64, lo_inode) == 8, "loop inode");
_Static_assert(offsetof(struct loop_info64, lo_rdevice) == 16, "loop rdevice");
_Static_assert(offsetof(struct loop_info64, lo_offset) == 24, "loop offset");
_Static_assert(offsetof(struct loop_info64, lo_sizelimit) == 32, "loop limit");
_Static_assert(offsetof(struct loop_info64, lo_number) == 40, "loop number");
_Static_assert(offsetof(struct loop_info64, lo_encrypt_type) == 44, "loop encryption");
_Static_assert(offsetof(struct loop_info64, lo_encrypt_key_size) == 48, "loop key size");
_Static_assert(offsetof(struct loop_info64, lo_flags) == 52, "loop flags");
_Static_assert(offsetof(struct loop_info64, lo_file_name) == 56, "loop filename");
_Static_assert(offsetof(struct loop_info64, lo_crypt_name) == 120, "loop crypt name");
_Static_assert(offsetof(struct loop_info64, lo_encrypt_key) == 184, "loop key");
_Static_assert(offsetof(struct loop_info64, lo_init) == 216, "loop init");
_Static_assert(WDIOC_GETSUPPORT == 0x80285700, "watchdog support request");
_Static_assert(WDIOC_GETTIMEOUT == 0x80045707, "watchdog timeout request");
_Static_assert(WDIOC_KEEPALIVE == 0x80045705, "watchdog keepalive request");
_Static_assert(LOOP_GET_STATUS64 == 0x4c05, "loop status request");
_Static_assert(LOOP_CLR_FD == 0x4c01, "loop detach request");
_Static_assert(LOOP_CTL_GET_FREE == 0x4c82, "loop free request");
_Static_assert(LOOP_SET_FD == 0x4c00, "loop attach request");
_Static_assert(LOOP_SET_STATUS64 == 0x4c04, "loop configuration request");
_Static_assert(LO_FLAGS_READ_ONLY == 1, "loop read-only flag");
#include <linux/dm-ioctl.h>
_Static_assert(sizeof(struct dm_ioctl) == 312, "DM header size");
_Static_assert(_Alignof(struct dm_ioctl) == 8, "DM header alignment");
_Static_assert(offsetof(struct dm_ioctl, version) == 0, "DM version");
_Static_assert(offsetof(struct dm_ioctl, data_size) == 12, "DM response length");
_Static_assert(offsetof(struct dm_ioctl, data_start) == 16, "DM data offset");
_Static_assert(offsetof(struct dm_ioctl, target_count) == 20, "DM count");
_Static_assert(offsetof(struct dm_ioctl, open_count) == 24, "DM users");
_Static_assert(offsetof(struct dm_ioctl, flags) == 28, "DM flags");
_Static_assert(offsetof(struct dm_ioctl, event_nr) == 32, "DM generation event");
_Static_assert(offsetof(struct dm_ioctl, dev) == 40, "DM device");
_Static_assert(offsetof(struct dm_ioctl, name) == 48, "DM name");
_Static_assert(offsetof(struct dm_ioctl, uuid) == 176, "DM UUID");
_Static_assert(offsetof(struct dm_ioctl, data) == 305, "DM minimum reply");
_Static_assert(sizeof(struct dm_target_spec) == 40, "DM target size");
_Static_assert(_Alignof(struct dm_target_spec) == 8, "DM target alignment");
_Static_assert(offsetof(struct dm_target_spec, sector_start) == 0, "DM target sector");
_Static_assert(offsetof(struct dm_target_spec, length) == 8, "DM target length");
_Static_assert(offsetof(struct dm_target_spec, status) == 16, "DM target status");
_Static_assert(offsetof(struct dm_target_spec, next) == 20, "DM target absolute next");
_Static_assert(offsetof(struct dm_target_spec, target_type) == 24, "DM target type");
_Static_assert(DM_DEV_STATUS == 0xc138fd07, "DM status request");
_Static_assert(DM_TABLE_STATUS == 0xc138fd0c, "DM table request");
_Static_assert(DM_DEV_REMOVE == 0xc138fd04, "DM remove request");
_Static_assert(DM_STATUS_TABLE_FLAG == (1 << 4), "DM table flags");
_Static_assert(DM_DEV_CREATE == 0xc138fd03, "DM create request");
_Static_assert(DM_TABLE_LOAD == 0xc138fd09, "DM load request");
_Static_assert(DM_DEV_SUSPEND == 0xc138fd06, "DM resume request");
_Static_assert(DM_READONLY_FLAG == 1, "DM read-only flag");
#include <linux/fs.h>
#include <linux/quota.h>
#include <sys/syscall.h>
_Static_assert(sizeof(struct fsxattr) == 28, "fsxattr size");
_Static_assert(_Alignof(struct fsxattr) == 4, "fsxattr alignment");
_Static_assert(offsetof(struct fsxattr, fsx_xflags) == 0, "fsxattr flags");
_Static_assert(offsetof(struct fsxattr, fsx_projid) == 12, "fsxattr project");
_Static_assert(FS_IOC_FSGETXATTR == 0x801c581f, "fsxattr get request");
_Static_assert(FS_IOC_FSSETXATTR == 0x401c5820, "fsxattr set request");
_Static_assert(sizeof(struct fstrim_range) == 24, "fstrim range size");
_Static_assert(_Alignof(struct fstrim_range) == 8, "fstrim range alignment");
_Static_assert(offsetof(struct fstrim_range, len) == 8, "fstrim range length");
_Static_assert(FITRIM == 0xc0185879, "trim request");
_Static_assert(FS_XFLAG_PROJINHERIT == 0x200, "project inherit flag");
_Static_assert(sizeof(struct if_dqblk) == 72, "dqblk size");
_Static_assert(_Alignof(struct if_dqblk) == 8, "dqblk alignment");
_Static_assert(offsetof(struct if_dqblk, dqb_bhardlimit) == 0, "dqblk block limit");
_Static_assert(offsetof(struct if_dqblk, dqb_curspace) == 16, "dqblk space");
_Static_assert(offsetof(struct if_dqblk, dqb_ihardlimit) == 24, "dqblk inode limit");
_Static_assert(offsetof(struct if_dqblk, dqb_curinodes) == 40, "dqblk inodes");
_Static_assert(offsetof(struct if_dqblk, dqb_valid) == 64, "dqblk valid");
_Static_assert(QCMD(Q_GETQUOTA, PRJQUOTA) == (int)0x80000702, "project quota get");
_Static_assert(QCMD(Q_SETQUOTA, PRJQUOTA) == (int)0x80000802, "project quota set");
_Static_assert(QIF_LIMITS == 5, "quota limit fields");
_Static_assert(SYS_quotactl_fd == 443, "quotactl_fd number");
#include <sys/timex.h>
_Static_assert(STA_UNSYNC == 0x0040, "clock unsynchronized status");
#include <mtd/ubi-user.h>
#include <mtd/mtd-abi.h>
_Static_assert(sizeof(struct ubi_mkvol_req) == 152, "UBI create size");
_Static_assert(_Alignof(struct ubi_mkvol_req) == 1, "UBI create alignment");
_Static_assert(offsetof(struct ubi_mkvol_req, alignment) == 4, "UBI create alignment field");
_Static_assert(offsetof(struct ubi_mkvol_req, bytes) == 8, "UBI create bytes");
_Static_assert(offsetof(struct ubi_mkvol_req, vol_type) == 16, "UBI create type");
_Static_assert(offsetof(struct ubi_mkvol_req, name_len) == 18, "UBI create name length");
_Static_assert(offsetof(struct ubi_mkvol_req, name) == 24, "UBI create name");
_Static_assert(sizeof(struct ubi_rnvol_req) == 4368, "UBI rename size");
_Static_assert(_Alignof(struct ubi_rnvol_req) == 1, "UBI rename alignment");
_Static_assert(offsetof(struct ubi_rnvol_req, ents) == 16, "UBI rename entries");
_Static_assert(sizeof(((struct ubi_rnvol_req *)0)->ents[0]) == 136, "UBI rename entry size");
_Static_assert(offsetof(struct ubi_rnvol_req, ents[1].name_len) == 156, "UBI rename name length");
_Static_assert(offsetof(struct ubi_rnvol_req, ents[1].name) == 160, "UBI rename name");
_Static_assert(sizeof(struct ubi_leb_change_req) == 16, "UBI LEB change size");
_Static_assert(offsetof(struct ubi_leb_change_req, bytes) == 4, "UBI LEB change bytes");
_Static_assert(offsetof(struct ubi_leb_change_req, padding) == 9, "UBI LEB change padding");
_Static_assert(sizeof(struct ubi_blkcreate_req) == 128, "UBI block size");
_Static_assert(UBI_MAX_VOLUME_NAME == 127 && UBI_MAX_RNVOL == 32, "UBI limits");
_Static_assert(UBI_VOL_NUM_AUTO == -1, "UBI automatic volume number");
_Static_assert(UBI_DYNAMIC_VOLUME == 3 && UBI_STATIC_VOLUME == 4, "UBI volume kinds");
_Static_assert(UBI_IOCMKVOL == 0x40986f00, "UBI create request");
_Static_assert(UBI_IOCRMVOL == 0x40046f01, "UBI remove request");
_Static_assert(UBI_IOCRNVOL == 0x51106f03, "UBI rename request");
_Static_assert(UBI_IOCVOLUP == 0x40084f00, "UBI update request");
_Static_assert(UBI_IOCEBCH == 0x40044f02, "UBI LEB change request");
_Static_assert(UBI_IOCVOLCRBLK == 0x40804f07, "UBI block create request");
_Static_assert(UBI_IOCVOLRMBLK == 0x4f08, "UBI block remove request");
_Static_assert(sizeof(__kernel_loff_t) == 8, "MTD offset size");
_Static_assert(MEMGETBADBLOCK == 0x40084d0b, "MTD bad block request");
