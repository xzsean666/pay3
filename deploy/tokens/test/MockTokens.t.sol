// SPDX-License-Identifier: MIT
pragma solidity ^0.8.24;

import "../src/MockUSDC.sol";
import "../src/MockUSDT.sol";

interface Vm {
    function sign(uint256 privateKey, bytes32 digest) external returns (uint8 v, bytes32 r, bytes32 s);
    function warp(uint256 newTimestamp) external;
}

contract MockTokensTest {
    Vm constant vm = Vm(address(bytes20(uint160(uint256(keccak256("hevm cheat code"))))));

    MockUSDC usdc;
    MockUSDT usdt;

    uint256 constant USER_PK = 0x1234567890123456789012345678901234567890123456789012345678901234;
    address user;
    address recipient = address(0x9999);

    function setUp() public {
        usdc = new MockUSDC(address(this), 1_000_000 * 10 ** 6);
        usdt = new MockUSDT(address(this), 1_000_000 * 10 ** 6);

        // Derive user address from private key
        bytes32 digest = keccak256("derive");
        (uint8 v, bytes32 r, bytes32 s) = vm.sign(USER_PK, digest);
        user = ecrecover(digest, v, r, s);

        // Fund user
        usdc.mint(user, 10_000 * 10 ** 6);
        usdt.mint(user, 10_000 * 10 ** 6);
    }

    function testUSDCAttributes() public view {
        require(keccak256(bytes(usdc.name())) == keccak256(bytes("USD Coin")), "wrong usdc name");
        require(keccak256(bytes(usdc.version())) == keccak256(bytes("2")), "wrong usdc version");
        require(keccak256(bytes(usdc.symbol())) == keccak256(bytes("USDC")), "wrong usdc symbol");
        require(usdc.decimals() == 6, "wrong usdc decimals");
    }

    function testUSDTAttributes() public view {
        require(keccak256(bytes(usdt.name())) == keccak256(bytes("(PoS) Tether USD")), "wrong usdt name");
        require(keccak256(bytes(usdt.version())) == keccak256(bytes("1")), "wrong usdt version");
        require(keccak256(bytes(usdt.symbol())) == keccak256(bytes("USDT")), "wrong usdt symbol");
        require(usdt.decimals() == 6, "wrong usdt decimals");
    }

    function testUSDCGaslessTransferWithAuthorization() public {
        uint256 amount = 100 * 10 ** 6;
        bytes32 nonce = keccak256("nonce-1");
        uint256 validAfter = 0;
        uint256 validBefore = block.timestamp + 3600;

        bytes32 structHash = keccak256(
            abi.encode(
                usdc.TRANSFER_WITH_AUTHORIZATION_TYPEHASH(),
                user,
                recipient,
                amount,
                validAfter,
                validBefore,
                nonce
            )
        );

        bytes32 digest = keccak256(
            abi.encodePacked("\x19\x01", usdc.DOMAIN_SEPARATOR(), structHash)
        );

        (uint8 v, bytes32 r, bytes32 s) = vm.sign(USER_PK, digest);

        // Relayer submits transferWithAuthorization
        usdc.transferWithAuthorization(user, recipient, amount, validAfter, validBefore, nonce, v, r, s);

        require(usdc.balanceOf(recipient) == amount, "recipient did not receive funds");
        require(usdc.authorizationState(user, nonce) == true, "nonce not marked used");
    }

    function testUSDTGaslessExecuteMetaTransaction() public {
        uint256 amount = 50 * 10 ** 6;
        bytes memory fnSig = abi.encodeWithSelector(usdt.transfer.selector, recipient, amount);

        uint256 nonce = usdt.getNonce(user);
        require(nonce == 0, "initial nonce must be 0");

        bytes32 structHash = keccak256(
            abi.encode(
                usdt.META_TRANSACTION_TYPEHASH(),
                nonce,
                user,
                keccak256(fnSig)
            )
        );

        bytes32 digest = keccak256(
            abi.encodePacked("\x19\x01", usdt.DOMAIN_SEPARATOR(), structHash)
        );

        (uint8 v, bytes32 r, bytes32 s) = vm.sign(USER_PK, digest);

        // Relayer submits executeMetaTransaction
        usdt.executeMetaTransaction(user, fnSig, r, s, v);

        require(usdt.balanceOf(recipient) == amount, "recipient did not receive funds");
        require(usdt.getNonce(user) == 1, "nonce did not increment");
    }
}
